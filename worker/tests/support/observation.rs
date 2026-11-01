//! Accumulates API watch evidence, including brief Ready transitions before runner exit.
use k8s_openapi::api::core::v1::{Pod, PodCondition};

#[derive(Default)]
pub struct Observation {
    pub conditions: Vec<PodCondition>,
    pub logs: String,
    pub exit_code: Option<i32>,
    pub pod_name: Option<String>,
    pod_uid: Option<String>,
}

impl Observation {
    pub fn record(&mut self, pod: &Pod) -> Result<(), String> {
        let uid = pod
            .metadata
            .uid
            .as_ref()
            .ok_or("pod UID missing from API evidence")?;
        if self
            .pod_uid
            .as_ref()
            .is_some_and(|previous| previous != uid)
        {
            return Err("fixture pod was replaced; refusing combined evidence".into());
        }
        self.pod_uid = Some(uid.clone());
        self.pod_name = pod.metadata.name.clone();
        if let Some(status) = &pod.status {
            self.conditions.extend(
                status
                    .conditions
                    .iter()
                    .flatten()
                    .filter(|condition| condition.type_ == "Ready")
                    .cloned(),
            );
            for container in status.container_statuses.iter().flatten() {
                if container.name != "runner"
                    || container.restart_count != 0
                    || container
                        .last_state
                        .as_ref()
                        .and_then(|state| state.terminated.as_ref())
                        .is_some()
                {
                    return Err(
                        "fixture container restarted or unexpected container observed".into(),
                    );
                }
                if let Some(terminated) = container
                    .state
                    .as_ref()
                    .and_then(|state| state.terminated.as_ref())
                {
                    // Keep a failed exit sticky. The adapter still needs to retrieve its logs.
                    if self.exit_code.is_none_or(|code| code == 0) {
                        self.exit_code = Some(terminated.exit_code);
                    }
                }
            }
        }
        Ok(())
    }
}
