//! Post-confirmation execution for `unisrv destroy`.
//!
//! Reuses `up`'s [`apply`] to delete every service/deployment (the plan is built
//! from an empty desired state, so the diff is all-deletes), then blocks until the
//! backend has actually drained the deployments before removing the environment.
//!
//! Why block here when `up` is fire-and-forget: the backend's `delete_environment`
//! uses `ON DELETE RESTRICT` and rejects (409) while any service, deployment, or
//! active instance still exists. Deleting a deployment only marks it `Deleting` and
//! triggers async teardown, so we must wait for the drain before deleting the env.

use std::time::Duration;

use anyhow::{Result, bail};
use unisrv_api::ApiClient;
use unisrv_api::models::HostResponse;
use uuid::Uuid;

use crate::commands::up::apply::apply;
use crate::commands::up::plan::{EnvAction, Plan};
use crate::progress::{Icon, Progress, Tone};

/// Poll cadence and ceiling for the deployment drain. Bounded so a stuck operator
/// can't hang the CLI forever — on timeout we error and the user reruns (destroy
/// is idempotent).
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const POLL_MAX_ATTEMPTS: usize = 60;

// The sleep seam lives next to `apply` (which shares it for network drain
// waits); re-exported here so destroy-side callers keep their import path.
pub use crate::commands::up::apply::{RealWaiter, Waiter};

/// Execute a destroy plan: apply all deletes, wait for deployments to drain, then
/// delete the environment. The plan's `env_action` must be `Use` — destroy never
/// creates an environment.
pub async fn destroy_execute(
    plan: Plan,
    client: &dyn ApiClient,
    hosts: &[HostResponse],
    waiter: &dyn Waiter,
    progress: &dyn Progress,
) -> Result<()> {
    let env_id = match &plan.env_action {
        EnvAction::Use(env) => env.id,
        EnvAction::Create(_) => {
            bail!("internal: destroy plan must target an existing environment, not create one")
        }
    };

    // Issue all the deletes (deployments → services → networks), reusing up's
    // apply — including its instance-drain-gated network deletes.
    apply(plan, client, hosts, waiter, progress).await?;

    // Deployments delete asynchronously; wait for them to drain before the
    // backend will let us remove the (now-empty) environment.
    poll_deployments_drained(client, env_id, waiter, POLL_MAX_ATTEMPTS, progress).await?;

    let step = progress.step(Icon::Environment, "Deleting environment");
    client.delete_environment(env_id).await?;
    step.finish(Tone::Remove, "environment destroyed");
    Ok(())
}

/// Poll `list_deployments` until the environment has no deployments left, sleeping
/// `POLL_INTERVAL` between attempts. Aborts early if a deployment appears that is
/// not in the `Deleting` state (someone created one concurrently). Errors on timeout.
pub async fn poll_deployments_drained(
    client: &dyn ApiClient,
    env_id: Uuid,
    waiter: &dyn Waiter,
    max_attempts: usize,
    progress: &dyn Progress,
) -> Result<()> {
    use crate::commands::up::apply::{Poll, PollOutcome, poll_until};

    let step = progress.step(Icon::Deployment, "Waiting for deployments to drain");
    let outcome = poll_until(waiter, POLL_INTERVAL, max_attempts, &step, async || {
        let list = client.list_deployments(env_id).await?;
        if list.deployments.is_empty() {
            return Ok(Poll::Done);
        }
        // Every remaining deployment must be on its way out. A deployment in any
        // other state means something created one while we were destroying —
        // bail rather than racing it; the user can rerun once it's settled.
        if let Some(d) = list.deployments.iter().find(|d| d.state.0 != "deleting") {
            bail!(
                "deployment {:?} is {:?}, not being deleted — the environment changed during \
                 destroy. Rerun `unisrv destroy` once it has settled.",
                d.name,
                d.state.0
            );
        }
        Ok(Poll::Pending(format!(
            "Draining {} deployment(s)…",
            list.deployments.len()
        )))
    })
    .await?;
    match outcome {
        PollOutcome::Done { rounds } => {
            let elapsed = rounds as u64 * POLL_INTERVAL.as_secs();
            step.finish(Tone::Remove, &format!("deployments drained ({elapsed}s)"));
            Ok(())
        }
        PollOutcome::TimedOut => bail!(
            "timed out after {}s waiting for deployments to drain. Rerun `unisrv destroy` to \
             finish.",
            max_attempts as u64 * POLL_INTERVAL.as_secs()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    use chrono::NaiveDateTime;
    use unisrv_api::models::{
        DeploymentConfiguration, HTTPLocation, HTTPLocationTarget, HTTPServiceConfig,
    };
    use unisrv_api::test_support::MockApiClient;

    use crate::commands::up::plan::{
        CurrentDeployment, CurrentService, DeploymentAction, ResolvedEnvironment, ServiceAction,
    };
    use crate::progress::SilentProgress;

    /// Test waiter: records how many times it was asked to sleep, never waits.
    struct CountingWaiter {
        sleeps: Mutex<usize>,
    }

    impl CountingWaiter {
        fn new() -> Self {
            Self {
                sleeps: Mutex::new(0),
            }
        }
        fn count(&self) -> usize {
            *self.sleeps.lock().unwrap()
        }
    }

    #[async_trait]
    impl Waiter for CountingWaiter {
        async fn sleep(&self, _dur: Duration) {
            *self.sleeps.lock().unwrap() += 1;
        }
    }

    fn http_config() -> HTTPServiceConfig {
        HTTPServiceConfig {
            allow_http: false,
            locations: vec![HTTPLocation {
                path: "/".into(),
                override_404: None,
                target: HTTPLocationTarget::Instance {
                    group: "default".into(),
                },
            }],
        }
    }

    fn dep_config() -> DeploymentConfiguration {
        DeploymentConfiguration {
            replicas: 1,
            region: "dev".into(),
            container_image: "nginx:1".into(),
            args: None,
            env: None,
            vcpu_ratio: 0.25,
            vcpu_count: 1,
            memory_mb: 256,
            instance_port: Some(80),
        }
    }

    fn current_service(name: &str) -> CurrentService {
        CurrentService {
            id: Uuid::new_v4(),
            name: name.into(),
            hosts: vec![],
            region: "dev".into(),
            configuration: http_config(),
        }
    }

    fn current_deployment(name: &str) -> CurrentDeployment {
        CurrentDeployment {
            network_binding: None,
            id: Uuid::new_v4(),
            name: name.into(),
            configuration: dep_config(),
            service_binding: None,
        }
    }

    /// A destroy plan: empty desired → every existing service/deployment is a Delete.
    fn destroy_plan(env_id: Uuid) -> Plan {
        Plan {
            network_actions: vec![],
            project: "demo".into(),
            env_action: EnvAction::Use(ResolvedEnvironment {
                id: env_id,
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![ServiceAction::Delete(current_service("web"))],
            deployment_actions: vec![DeploymentAction::Delete(current_deployment("web"))],
            instance_stops: vec![],
        }
    }

    #[tokio::test]
    async fn destroy_deletes_network_after_stops_and_before_env_delete() {
        // Networks go last in apply (their blockers can be the standalone
        // instances the stop pass tears down), and the env delete happens only
        // after the whole apply — the env CASCADE is a backstop, not the
        // mechanism.
        use crate::commands::up::plan::{CurrentNetwork, InstanceStop, NetworkAction};
        use unisrv_api::models::NetworkResponse;

        let env_id = Uuid::new_v4();
        let net_id = Uuid::new_v4();
        let inst_id = Uuid::new_v4();

        let mut plan = destroy_plan(env_id);
        plan.network_actions = vec![NetworkAction::Delete(CurrentNetwork {
            id: net_id,
            name: "internal".into(),
            ipv4_cidr: "10.0.0.0/16".into(),
        })];
        plan.instance_stops = vec![InstanceStop {
            id: inst_id,
            name: Some("redis-cache".into()),
        }];

        let client = MockApiClient::logged_in()
            .push_delete_deployment(Ok(()))
            .push_delete_service(Ok(()))
            .push_deprovision_instance(Ok(()))
            .push_get_network(Ok(NetworkResponse {
                id: net_id,
                environment_id: env_id,
                name: "internal".into(),
                ipv4_cidr: "10.0.0.0/16".into(),
                created_at: NaiveDateTime::default(),
                instances: vec![],
            }))
            .push_delete_network(Ok(()))
            .with_list_deployments(Ok(empty_deployments()))
            .push_delete_environment(Ok(()));
        let waiter = CountingWaiter::new();

        destroy_execute(plan, &client, &[], &waiter, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.delete_network_calls, vec![(env_id, net_id)]);
        let order = &calls.call_order;
        let pos = |n: &str| order.iter().position(|m| *m == n).unwrap();
        assert!(
            pos("deprovision_instance") < pos("delete_network"),
            "{order:?}"
        );
        assert!(
            pos("delete_network") < pos("delete_environment"),
            "{order:?}"
        );
    }

    fn empty_deployments() -> unisrv_api::models::DeploymentListResponse {
        unisrv_api::models::DeploymentListResponse {
            deployments: vec![],
        }
    }

    fn deployments_in_state(state: &str) -> unisrv_api::models::DeploymentListResponse {
        unisrv_api::models::DeploymentListResponse {
            deployments: vec![unisrv_api::models::DeploymentListEntry {
                id: Uuid::new_v4(),
                name: "web".into(),
                state: unisrv_api::models::DeploymentState(state.into()),
                replicas: 1,
                container_image: "nginx:1".into(),
                created_at: NaiveDateTime::default(),
            }],
        }
    }

    #[tokio::test]
    async fn destroys_in_order_deployments_services_then_environment() {
        let env_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_delete_deployment(Ok(()))
            .push_delete_service(Ok(()))
            // Drain poll: deployments already gone on first list.
            .with_list_deployments(Ok(empty_deployments()))
            .push_delete_environment(Ok(()));
        let waiter = CountingWaiter::new();

        destroy_execute(destroy_plan(env_id), &client, &[], &waiter, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.delete_deployment_calls.len(), 1);
        assert_eq!(calls.delete_service_calls.len(), 1);
        assert_eq!(calls.delete_environment_calls, vec![env_id]);

        // Ordering invariant: all deployment deletes precede service deletes,
        // the drain poll runs after the deletes, and the env is deleted last.
        let order = &calls.call_order;
        let pos = |name: &str| order.iter().position(|m| *m == name).unwrap();
        let rpos = |name: &str| order.iter().rposition(|m| *m == name).unwrap();
        assert!(
            rpos("delete_deployment") < pos("delete_service"),
            "{order:?}"
        );
        assert!(
            rpos("delete_service") < pos("list_deployments"),
            "{order:?}"
        );
        assert!(
            rpos("list_deployments") < pos("delete_environment"),
            "{order:?}"
        );
    }

    #[tokio::test]
    async fn poll_drains_after_several_attempts() {
        let env_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_list_deployments(Ok(deployments_in_state("deleting")))
            .with_list_deployments(Ok(deployments_in_state("deleting")))
            .with_list_deployments(Ok(empty_deployments()));
        let waiter = CountingWaiter::new();

        poll_deployments_drained(&client, env_id, &waiter, 5, &SilentProgress)
            .await
            .unwrap();

        // Slept once after each of the two non-empty lists; the empty list returns.
        assert_eq!(waiter.count(), 2);
        assert_eq!(client.calls.lock().unwrap().list_deployments_calls.len(), 3);
    }

    #[tokio::test]
    async fn aborts_and_keeps_env_when_a_non_deleting_deployment_appears() {
        let env_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_delete_deployment(Ok(()))
            .push_delete_service(Ok(()))
            // A fresh deployment showed up mid-destroy (someone ran `up`).
            .with_list_deployments(Ok(deployments_in_state("in_sync")))
            .push_delete_environment(Ok(()));
        let waiter = CountingWaiter::new();

        let err = destroy_execute(destroy_plan(env_id), &client, &[], &waiter, &SilentProgress)
            .await
            .unwrap_err();

        let msg = format!("{err:#}");
        assert!(msg.contains("destroy"), "msg: {msg}");
        // The environment must NOT be deleted while a live deployment exists.
        assert!(
            client
                .calls
                .lock()
                .unwrap()
                .delete_environment_calls
                .is_empty(),
            "environment was deleted despite an active deployment"
        );
    }

    #[tokio::test]
    async fn poll_times_out_when_deployments_never_drain() {
        let env_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_list_deployments(Ok(deployments_in_state("deleting")))
            .with_list_deployments(Ok(deployments_in_state("deleting")))
            .with_list_deployments(Ok(deployments_in_state("deleting")));
        let waiter = CountingWaiter::new();

        let err = poll_deployments_drained(&client, env_id, &waiter, 3, &SilentProgress)
            .await
            .unwrap_err();

        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
        assert_eq!(waiter.count(), 3);
    }
}
