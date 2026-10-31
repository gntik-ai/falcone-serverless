//! First-run evidence collection at the Kubernetes Pod API boundary.
use super::was01::{Plan, first_run};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api,
    api::{ListParams, LogParams},
    core::ObjectList,
};
use std::time::{Duration, Instant};
const API_ERROR: &str = "Kubernetes observation failed; response and credentials suppressed";

pub(super) trait PodClient {
    async fn list(&self, params: &ListParams) -> Result<ObjectList<Pod>, kube::Error>;
    async fn get(&self, name: &str) -> Result<Pod, kube::Error>;
    async fn logs(&self, name: &str, params: &LogParams) -> Result<String, kube::Error>;
}

impl PodClient for Api<Pod> {
    async fn list(&self, params: &ListParams) -> Result<ObjectList<Pod>, kube::Error> {
        Api::list(self, params).await
    }
    async fn get(&self, name: &str) -> Result<Pod, kube::Error> {
        Api::get(self, name).await
    }
    async fn logs(&self, name: &str, params: &LogParams) -> Result<String, kube::Error> {
        Api::logs(self, name, params).await
    }
}

pub(super) async fn observe(api: &impl PodClient, plan: &Plan) -> Result<String, String> {
    let params = ListParams::default().labels(&format!("falcone.test/run={}", plan.run));
    let mut first_identity = None;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let pods = api.list(&params).await.map_err(|_| API_ERROR)?;
        if pods.items.len() > 1 {
            return Err("more than one qualification pod exists".into());
        }
        if let Some(pod) = pods.items.first() {
            let object = serde_json::to_value(pod).map_err(|_| API_ERROR)?;
            plan.validate_pod(&object)?;
            if let Some(first) = first_run(&object)? {
                let uid = pod
                    .metadata
                    .uid
                    .as_deref()
                    .filter(|uid| !uid.is_empty())
                    .ok_or("pod UID missing")?;
                let identity = (uid.to_string(), first.container_id.clone());
                if first_identity
                    .as_ref()
                    .is_some_and(|expected| expected != &identity)
                {
                    return Err("runner replaced between first-run evidence polls".into());
                }
                first_identity = Some(identity);
                let name = pod.metadata.name.as_deref().ok_or("pod name missing")?;
                let logs = api
                    .logs(
                        name,
                        &LogParams {
                            container: Some("runner".into()),
                            limit_bytes: Some(65536),
                            previous: first.previous,
                            ..Default::default()
                        },
                    )
                    .await;
                let after = api.get(name).await.map_err(|_| API_ERROR)?;
                let after_value = serde_json::to_value(&after).map_err(|_| API_ERROR)?;
                plan.validate_pod(&after_value)?;
                let after_first =
                    first_run(&after_value)?.ok_or("first-run evidence disappeared")?;
                if after.metadata.uid != pod.metadata.uid
                    || after_first.container_id != first.container_id
                {
                    return Err("runner replaced while collecting first-run evidence".into());
                }
                if after_first.previous == first.previous {
                    return logs.map_err(|_| API_ERROR.into());
                }
                if first.previous {
                    return Err("runner restart status regressed".into());
                }
                // A restart raced current-log retrieval. Discard those logs and
                // fetch previous=true on the next poll, with the same first ID.
            }
        }
        if Instant::now() >= deadline {
            return Err("runner startup/first-run completion timed out".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::super::Namespace;
    use super::*;
    use serde_json::json;
    use std::{cell::RefCell, collections::VecDeque};

    struct PodApi {
        lists: RefCell<VecDeque<Pod>>,
        reads: RefCell<VecDeque<Pod>>,
        log_sources: RefCell<Vec<bool>>,
    }

    fn plan() -> Plan {
        Plan::new(
            Namespace::new("fs-test-shared").unwrap(),
            "fs-test-was01-observe",
            &format!("registry.invalid/was01@sha256:{}", "a".repeat(64)),
            1000770000,
        )
        .unwrap()
    }

    fn pod(plan: &Plan, uid: &str, previous: bool) -> Pod {
        let mut status = json!({"name":"runner","restartCount":0,"containerID":"containerd://first","state":{"terminated":{"exitCode":0}}});
        if previous {
            status = json!({"name":"runner","restartCount":1,"containerID":"containerd://second","state":{"running":{}},"lastState":{"terminated":{"exitCode":0,"containerID":"containerd://first"}}});
        }
        serde_json::from_value(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"runner-pod","namespace":plan.namespace.name(),"uid":uid,"labels":{"falcone.test/run":plan.run}},"spec":plan.fixture()[1]["spec"]["template"]["spec"],"status":{"containerStatuses":[status]}})).unwrap()
    }

    impl PodApi {
        fn new(lists: Vec<Pod>, reads: Vec<Pod>) -> Self {
            Self {
                lists: RefCell::new(lists.into()),
                reads: RefCell::new(reads.into()),
                log_sources: RefCell::new(vec![]),
            }
        }
    }

    impl PodClient for PodApi {
        async fn list(&self, params: &ListParams) -> Result<ObjectList<Pod>, kube::Error> {
            assert_eq!(
                params.label_selector.as_deref(),
                Some("falcone.test/run=fs-test-was01-observe")
            );
            let pod = self
                .lists
                .borrow_mut()
                .pop_front()
                .expect("unexpected list");
            Ok(serde_json::from_value(
                json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[pod]}),
            )
            .unwrap())
        }
        async fn get(&self, name: &str) -> Result<Pod, kube::Error> {
            assert_eq!(name, "runner-pod");
            Ok(self.reads.borrow_mut().pop_front().expect("unexpected get"))
        }
        async fn logs(&self, name: &str, params: &LogParams) -> Result<String, kube::Error> {
            assert_eq!(name, "runner-pod");
            assert_eq!(params.container.as_deref(), Some("runner"));
            assert_eq!(params.limit_bytes, Some(65536));
            self.log_sources.borrow_mut().push(params.previous);
            Ok(if params.previous {
                "first-run logs"
            } else {
                "current-run logs"
            }
            .into())
        }
    }

    #[tokio::test]
    async fn observation_rejects_pod_replacement_between_log_source_retries() {
        let plan = plan();
        let api = PodApi::new(
            vec![
                pod(&plan, "first-pod", false),
                pod(&plan, "replacement-pod", true),
            ],
            vec![
                pod(&plan, "first-pod", true),
                pod(&plan, "replacement-pod", true),
            ],
        );
        assert!(observe(&api, &plan).await.is_err());
    }
    #[tokio::test]
    async fn observation_uses_the_correct_log_endpoint_for_the_first_run() {
        let plan = plan();
        for previous in [false, true] {
            let api = PodApi::new(
                vec![pod(&plan, "first-pod", previous)],
                vec![pod(&plan, "first-pod", previous)],
            );
            assert_eq!(
                observe(&api, &plan).await.unwrap(),
                if previous {
                    "first-run logs"
                } else {
                    "current-run logs"
                }
            );
            assert_eq!(*api.log_sources.borrow(), [previous]);
        }
    }

    #[tokio::test]
    async fn observation_discards_current_logs_if_a_restart_races_the_fetch() {
        let plan = plan();
        let api = PodApi::new(
            vec![
                pod(&plan, "first-pod", false),
                pod(&plan, "first-pod", true),
            ],
            vec![pod(&plan, "first-pod", true), pod(&plan, "first-pod", true)],
        );
        assert_eq!(observe(&api, &plan).await.unwrap(), "first-run logs");
        assert_eq!(*api.log_sources.borrow(), [false, true]);
    }

    #[tokio::test]
    async fn observation_revalidates_pod_identity_posture_and_first_run_after_logs() {
        let plan = plan();
        for change in [
            "replacement",
            "second-restart",
            "different-container",
            "posture",
        ] {
            let mut changed = pod(&plan, "first-pod", true);
            match change {
                "replacement" => changed.metadata.uid = Some("replacement-pod".into()),
                "second-restart" => {
                    changed
                        .status
                        .as_mut()
                        .unwrap()
                        .container_statuses
                        .as_mut()
                        .unwrap()[0]
                        .restart_count = 2
                }
                "different-container" => {
                    changed
                        .status
                        .as_mut()
                        .unwrap()
                        .container_statuses
                        .as_mut()
                        .unwrap()[0]
                        .last_state
                        .as_mut()
                        .unwrap()
                        .terminated
                        .as_mut()
                        .unwrap()
                        .container_id = Some("containerd://replacement".into())
                }
                "posture" => {
                    changed
                        .spec
                        .as_mut()
                        .unwrap()
                        .security_context
                        .as_mut()
                        .unwrap()
                        .run_as_user = Some(0)
                }
                _ => unreachable!(),
            }
            let api = PodApi::new(vec![pod(&plan, "first-pod", true)], vec![changed]);
            assert!(observe(&api, &plan).await.is_err(), "{change}");
            assert_eq!(*api.log_sources.borrow(), [true]);
        }
    }
}
