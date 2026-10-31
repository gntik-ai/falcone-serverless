//! Shared-mode cleanup at the Kubernetes API boundary (no credential handling).
use super::Namespace;
use kube::{
    api::{DeleteParams, DynamicObject, ListParams, Preconditions},
    core::ObjectList,
};
use std::time::{Duration, Instant};

pub(super) const OWNED_KINDS: [(&str, &str, &str); 6] = [
    ("apps", "v1", "Deployment"),
    ("apps", "v1", "ReplicaSet"),
    ("", "v1", "Pod"),
    ("rbac.authorization.k8s.io", "v1", "RoleBinding"),
    ("rbac.authorization.k8s.io", "v1", "Role"),
    ("", "v1", "ServiceAccount"),
];

// Mock only these external Kubernetes operations; all selection, ownership checks,
// delete preconditions and lifecycle decisions remain in the production loop.
pub(super) trait CleanupClient {
    async fn list(
        &self,
        namespace: &Namespace,
        resource: (&str, &str, &str),
        params: &ListParams,
    ) -> Result<ObjectList<DynamicObject>, kube::Error>;
    async fn get(
        &self,
        namespace: &Namespace,
        resource: (&str, &str, &str),
        name: &str,
    ) -> Result<Option<DynamicObject>, kube::Error>;
    async fn delete(
        &self,
        namespace: &Namespace,
        resource: (&str, &str, &str),
        name: &str,
        params: &DeleteParams,
    ) -> Result<(), kube::Error>;
}

pub(super) async fn cleanup_run(
    client: &impl CleanupClient,
    namespace: &Namespace,
    run: &str,
) -> Result<(), String> {
    Namespace::new(run)?;
    let params = ListParams::default().labels(&format!("falcone.test/run={run}"));
    let mut errors = Vec::new();
    for (group, version, kind) in OWNED_KINDS {
        let resource = (group, version, kind);
        let list = match client.list(namespace, resource, &params).await {
            Ok(list) => list,
            Err(_) => {
                errors.push(format!("{kind}: list failed (response suppressed)"));
                continue;
            }
        };
        for object in list.items {
            if let Err(error) = delete_owned(client, namespace, resource, run, object).await {
                errors.push(format!("{kind}: {error}"));
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let mut remaining = false;
        for (group, version, kind) in OWNED_KINDS {
            let resource = (group, version, kind);
            match client.list(namespace, resource, &params).await {
                Ok(list) => remaining |= list.items.iter().any(|object| owned_by(object, run)),
                Err(_) => errors.push(format!(
                    "{kind}: cleanup verification failed (response suppressed)"
                )),
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        if !remaining {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("run-labelled objects remain after cleanup".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn owned_by(object: &DynamicObject, run: &str) -> bool {
    object
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("falcone.test/run"))
        .map(String::as_str)
        == Some(run)
}

async fn delete_owned(
    client: &impl CleanupClient,
    namespace: &Namespace,
    resource: (&str, &str, &str),
    run: &str,
    mut object: DynamicObject,
) -> Result<(), &'static str> {
    if !owned_by(&object, run) {
        return Ok(());
    }
    let name = object
        .metadata
        .name
        .clone()
        .ok_or("owned object name missing")?;
    let uid = object
        .metadata
        .uid
        .clone()
        .ok_or("owned object UID missing")?;
    // Bound retries: a permanently conflicting object must not prevent RBAC cleanup.
    for _ in 0..5 {
        if !owned_by(&object, run) {
            return Ok(());
        }
        if object.metadata.uid.as_deref() != Some(&uid)
            || object.metadata.name.as_deref() != Some(&name)
        {
            return Err("owned object replaced during cleanup");
        }
        let version = object
            .metadata
            .resource_version
            .clone()
            .ok_or("owned object version missing")?;
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(uid.clone()),
                resource_version: Some(version),
            }),
            ..DeleteParams::background()
        };
        match client.delete(namespace, resource, &name, &params).await {
            Ok(()) => return Ok(()),
            Err(kube::Error::Api(error)) if error.code == 404 => return Ok(()),
            Err(kube::Error::Api(error)) if error.code == 409 => {
                // Re-read and re-check the label, but never adopt a replacement UID.
                match client.get(namespace, resource, &name).await {
                    Ok(Some(current)) => object = current,
                    Ok(None) => return Ok(()),
                    Err(_) => return Err("conflict re-read failed (response suppressed)"),
                }
            }
            Err(_) => return Err("delete failed (response suppressed)"),
        }
    }
    Err("delete preconditions kept conflicting")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{cell::RefCell, collections::BTreeMap};

    const RUN: &str = "fs-test-was01-cleanup";
    const NAMESPACE: &str = "fs-test-shared";
    const KINDS: [&str; 6] = [
        "Deployment",
        "ReplicaSet",
        "Pod",
        "RoleBinding",
        "Role",
        "ServiceAccount",
    ];

    struct ApiFake {
        objects: RefCell<BTreeMap<String, DynamicObject>>,
        listed: RefCell<Vec<String>>,
        deleted: RefCell<Vec<String>>,
        failures: BTreeMap<String, u16>,
        conflicts: RefCell<BTreeMap<String, Option<DynamicObject>>>,
        preconditions: RefCell<Vec<(String, String)>>,
        reads: RefCell<Vec<String>>,
    }

    fn object(kind: &str, name: &str, run: &str) -> DynamicObject {
        serde_json::from_value(json!({"apiVersion":"v1","kind":kind,"metadata":{
            "name":name,"namespace":NAMESPACE,"uid":format!("uid-{name}"),
            "resourceVersion":"7","labels":{"falcone.test/run":run}
        }}))
        .unwrap()
    }

    fn error(code: u16) -> kube::Error {
        kube::Error::Api(Box::new(serde_json::from_value(json!({
            "status":"Failure", "message":"credential-output-canary", "reason":"test rejection", "code":code
        })).unwrap()))
    }

    impl ApiFake {
        fn new(failures: &[(&str, u16)]) -> Self {
            Self {
                objects: RefCell::new(
                    KINDS
                        .into_iter()
                        .map(|kind| {
                            let name = format!("owned-{kind}");
                            (name.clone(), object(kind, &name, RUN))
                        })
                        .collect(),
                ),
                conflicts: RefCell::new(BTreeMap::new()),
                preconditions: RefCell::new(vec![]),
                reads: RefCell::new(vec![]),
                listed: RefCell::new(vec![]),
                deleted: RefCell::new(vec![]),
                failures: failures
                    .iter()
                    .map(|(name, code)| ((*name).into(), *code))
                    .collect(),
            }
        }
    }

    impl CleanupClient for ApiFake {
        async fn list(
            &self,
            namespace: &Namespace,
            resource: (&str, &str, &str),
            params: &ListParams,
        ) -> Result<ObjectList<DynamicObject>, kube::Error> {
            assert_eq!(namespace.name(), NAMESPACE);
            assert_eq!(
                params.label_selector.as_deref(),
                Some("falcone.test/run=fs-test-was01-cleanup")
            );
            let (group, version, kind) = resource;
            assert_eq!(version, "v1");
            assert_eq!(
                group,
                match kind {
                    "Deployment" | "ReplicaSet" => "apps",
                    "Role" | "RoleBinding" => "rbac.authorization.k8s.io",
                    "Pod" | "ServiceAccount" => "",
                    _ => panic!("unexpected resource"),
                }
            );
            self.listed.borrow_mut().push(kind.into());
            if let Some(code) = self.failures.get(&format!("list-{kind}")) {
                return Err(error(*code));
            }
            Ok(serde_json::from_value(json!({"apiVersion":"v1","kind":"List","metadata":{},"items":self.objects.borrow().values().filter(|object| object.types.as_ref().unwrap().kind == kind).collect::<Vec<_>>()})).unwrap())
        }
        async fn get(
            &self,
            namespace: &Namespace,
            resource: (&str, &str, &str),
            name: &str,
        ) -> Result<Option<DynamicObject>, kube::Error> {
            assert_eq!(namespace.name(), NAMESPACE);
            self.reads.borrow_mut().push(name.into());
            let object = self.objects.borrow().get(name).cloned();
            if let Some(object) = &object {
                assert_eq!(object.types.as_ref().unwrap().kind, resource.2);
            }
            Ok(object)
        }
        async fn delete(
            &self,
            namespace: &Namespace,
            resource: (&str, &str, &str),
            name: &str,
            params: &DeleteParams,
        ) -> Result<(), kube::Error> {
            assert_eq!(namespace.name(), NAMESPACE);
            let objects = self.objects.borrow();
            let object = objects.get(name).unwrap();
            assert_eq!(object.types.as_ref().unwrap().kind, resource.2);
            let preconditions = params
                .preconditions
                .as_ref()
                .expect("delete must be conditional");
            assert_eq!(preconditions.uid, object.metadata.uid);
            assert_eq!(
                preconditions.resource_version,
                object.metadata.resource_version
            );
            self.preconditions.borrow_mut().push((
                preconditions.uid.clone().unwrap(),
                preconditions.resource_version.clone().unwrap(),
            ));
            drop(objects);
            self.deleted.borrow_mut().push(resource.2.into());
            if let Some(changed) = self.conflicts.borrow_mut().remove(name) {
                if let Some(changed) = changed {
                    self.objects.borrow_mut().insert(name.into(), changed);
                } else {
                    self.objects.borrow_mut().remove(name);
                }
                return Err(error(409));
            }
            if let Some(code) = self.failures.get(name) {
                if *code == 404 {
                    self.objects.borrow_mut().remove(name);
                }
                return Err(error(*code));
            }
            self.objects.borrow_mut().remove(name);
            Ok(())
        }
    }

    #[tokio::test]
    async fn cleanup_attempts_all_kinds_after_delete_errors_and_suppresses_raw_errors() {
        let api = ApiFake::new(&[("owned-Deployment", 403), ("owned-Pod", 500)]);
        let result = cleanup_run(&api, &Namespace::new(NAMESPACE).unwrap(), RUN).await;
        assert_eq!(*api.deleted.borrow(), KINDS);
        let error = result.unwrap_err();
        assert!(!error.contains("credential-output-canary"));
        assert!(error.contains("Deployment:") && error.contains("Pod:"));
        assert_eq!(api.objects.borrow().len(), 2);
    }
    #[tokio::test]
    async fn cleanup_retries_a_conflict_with_fresh_version_and_original_uid() {
        let api = ApiFake::new(&[]);
        let mut changed = api.objects.borrow()["owned-Pod"].clone();
        changed.metadata.resource_version = Some("8".into());
        api.conflicts
            .borrow_mut()
            .insert("owned-Pod".into(), Some(changed));
        cleanup_run(&api, &Namespace::new(NAMESPACE).unwrap(), RUN)
            .await
            .unwrap();
        assert_eq!(*api.reads.borrow(), ["owned-Pod"]);
        let pod_requests: Vec<_> = api
            .preconditions
            .borrow()
            .iter()
            .filter(|(uid, _)| uid == "uid-owned-Pod")
            .cloned()
            .collect();
        assert_eq!(
            pod_requests,
            [
                ("uid-owned-Pod".into(), "7".into()),
                ("uid-owned-Pod".into(), "8".into())
            ]
        );
        assert!(api.objects.borrow().is_empty());
    }

    #[tokio::test]
    async fn cleanup_deletes_only_run_owned_objects_for_every_resource_kind() {
        let api = ApiFake::new(&[]);
        for kind in KINDS {
            let name = format!("foreign-{kind}");
            api.objects
                .borrow_mut()
                .insert(name.clone(), object(kind, &name, "foreign-run"));
            let name = format!("unlabelled-{kind}");
            let mut unlabelled = object(kind, &name, RUN);
            unlabelled.metadata.labels = None;
            api.objects.borrow_mut().insert(name, unlabelled);
        }
        cleanup_run(&api, &Namespace::new(NAMESPACE).unwrap(), RUN)
            .await
            .unwrap();
        assert_eq!(*api.deleted.borrow(), KINDS);
        assert_eq!(api.objects.borrow().len(), 12);
        assert!(
            api.objects
                .borrow()
                .keys()
                .all(|name| name.starts_with("foreign-") || name.starts_with("unlabelled-"))
        );
    }

    #[tokio::test]
    async fn cleanup_never_deletes_objects_relabelled_or_replaced_during_a_conflict() {
        for change in ["foreign-label", "no-label", "replacement", "gone"] {
            let api = ApiFake::new(&[]);
            let mut changed = api.objects.borrow()["owned-Pod"].clone();
            match change {
                "foreign-label" => {
                    changed
                        .metadata
                        .labels
                        .as_mut()
                        .unwrap()
                        .insert("falcone.test/run".into(), "foreign-run".into());
                }
                "no-label" => changed.metadata.labels = None,
                "replacement" => changed.metadata.uid = Some("replacement-uid".into()),
                _ => {}
            }
            api.conflicts.borrow_mut().insert(
                "owned-Pod".into(),
                if change == "gone" {
                    None
                } else {
                    Some(changed)
                },
            );
            let result = cleanup_run(&api, &Namespace::new(NAMESPACE).unwrap(), RUN).await;
            assert_eq!(result.is_err(), change == "replacement", "{change}");
            assert_eq!(*api.deleted.borrow(), KINDS, "{change}");
            assert_eq!(*api.reads.borrow(), ["owned-Pod"], "{change}");
            assert_eq!(api.objects.borrow().len(), usize::from(change != "gone"));
        }
    }

    #[tokio::test]
    async fn cleanup_continues_after_list_failure_and_bounds_persistent_conflicts() {
        let api = ApiFake::new(&[("list-ReplicaSet", 403), ("owned-Pod", 409)]);
        let error = cleanup_run(&api, &Namespace::new(NAMESPACE).unwrap(), RUN)
            .await
            .unwrap_err();
        assert!(error.contains("ReplicaSet:") && error.contains("Pod:"));
        assert!(!error.contains("credential-output-canary"));
        assert_eq!(*api.listed.borrow(), KINDS);
        assert_eq!(
            *api.deleted.borrow(),
            [
                "Deployment",
                "Pod",
                "Pod",
                "Pod",
                "Pod",
                "Pod",
                "RoleBinding",
                "Role",
                "ServiceAccount"
            ]
        );
        assert_eq!(api.objects.borrow().len(), 2);
    }

    #[tokio::test]
    async fn cleanup_tolerates_an_object_disappearing_before_delete() {
        let api = ApiFake::new(&[("owned-Pod", 404)]);
        cleanup_run(&api, &Namespace::new(NAMESPACE).unwrap(), RUN)
            .await
            .unwrap();
        assert_eq!(*api.deleted.borrow(), KINDS);
        assert!(api.objects.borrow().is_empty());
    }
}
