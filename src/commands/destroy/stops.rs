//! Select which instances `destroy` must tear down explicitly.
//!
//! Deployment-managed instances drain on their own when their deployment is
//! deleted, so destroy only needs to deprovision *standalone* instances (those
//! with no owning deployment). Instances already in a terminal state are skipped —
//! they no longer count against the environment's delete guard.

use unisrv_api::models::InstanceListEntry;

use crate::commands::up::plan::InstanceStop;

/// Instance states the backend treats as finished. An instance in one of these
/// won't block environment deletion, so there's nothing to stop.
const TERMINAL_STATES: &[&str] = &["exited", "failed"];

/// From a full instance listing, pick the standalone, still-active instances that
/// destroy must deprovision directly.
pub fn select_instance_stops(instances: &[InstanceListEntry]) -> Vec<InstanceStop> {
    instances
        .iter()
        .filter(|i| i.deployment.is_none())
        .filter(|i| !TERMINAL_STATES.contains(&i.state.0.as_str()))
        .map(|i| InstanceStop {
            id: i.id,
            name: i.name.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use unisrv_api::models::{DeploymentInfo, InstanceState};
    use uuid::Uuid;

    fn entry(state: &str, deployment: Option<DeploymentInfo>) -> InstanceListEntry {
        InstanceListEntry {
            id: Uuid::new_v4(),
            name: Some("worker".into()),
            state: InstanceState(state.into()),
            container_image: "busybox".into(),
            created_at: NaiveDateTime::default(),
            deployment,
        }
    }

    fn dep_info() -> DeploymentInfo {
        DeploymentInfo {
            id: Uuid::new_v4(),
            name: "web".into(),
        }
    }

    #[test]
    fn includes_standalone_active_instances() {
        let inst = entry("running", None);
        let id = inst.id;
        let stops = select_instance_stops(&[inst]);
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].id, id);
    }

    #[test]
    fn excludes_deployment_managed_instances() {
        let stops = select_instance_stops(&[entry("running", Some(dep_info()))]);
        assert!(
            stops.is_empty(),
            "deployment-managed instances drain via their deployment"
        );
    }

    #[test]
    fn excludes_terminal_standalone_instances() {
        let stops = select_instance_stops(&[entry("exited", None), entry("failed", None)]);
        assert!(
            stops.is_empty(),
            "terminal instances don't block env deletion"
        );
    }
}
