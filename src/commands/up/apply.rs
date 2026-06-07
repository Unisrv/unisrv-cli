//! Execute a [`Plan`] against the API.
//!
//! Ordering rationale (see plan.rs for backend constraints):
//! 1. Create env (if EnvAction::Create).
//! 2. Create new services.
//! 3. Update services (config-only).
//! 4. Unlink hosts dropped by *surviving* (updated) services.
//! 5. Delete deployments being deleted *or recreated* (frees bindings).
//! 6. Recreate services: delete old, then create new (new IDs).
//! 7. Create deployments (new + recreated, looks up service_id by name).
//! 8. Update deployments (config-only).
//! 9. Delete services being fully removed (cascade-frees their hosts).
//! 10. Link hosts to their desired services.
//!
//! HOST INVARIANT: every host-FREEING step (unlink pass #4; delete cascade #9)
//! runs before the host-LINKING step (#10). A host can only be linked while
//! unbound, so freeing must precede binding — otherwise the backend 409s. Deleted
//! services are freed by the DB cascade, not an explicit unlink. Do not reorder.
//!
//! No rollback. On error, return immediately. Reconcile re-run will pick up.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use unisrv_api::ApiClient;
use unisrv_api::models::{
    CreateDeploymentRequest, DeploymentServiceBinding, HostResponse, ServiceProvisionRequest,
    UpdateDeploymentRequest,
};
use uuid::Uuid;

use super::desired::{DesiredDeployment, DesiredService, DesiredServiceBinding};
use super::diff::service::host_link_unlink;
use super::plan::{
    CurrentDeployment, CurrentService, DeploymentAction, EnvAction, Plan, ServiceAction,
};
use super::render::{Reachability, render_reachability};
use crate::commands::host::normalize_host;
use crate::progress::{Icon, Progress, Tone};

pub async fn apply(
    plan: Plan,
    client: &dyn ApiClient,
    hosts: &[HostResponse],
    progress: &dyn Progress,
) -> Result<()> {
    // Host string → claimed-host id, for resolving link/unlink targets. Hosts
    // are user-global (not env-scoped), so this is just an id dictionary.
    let host_ids: BTreeMap<String, Uuid> = hosts
        .iter()
        .map(|h| (normalize_host(&h.host), h.id))
        .collect();

    // Compute host reconciliation from the service actions up front (before
    // partitioning consumes them). Deleted services need no explicit unlink —
    // `ON DELETE SET NULL` clears their bindings. Recreated services have their
    // links wiped by the delete half, so all desired hosts are re-linked.
    let mut host_unlinks: Vec<(Uuid, Vec<String>)> = Vec::new();
    let mut host_links: Vec<(String, Vec<String>)> = Vec::new();
    for action in &plan.service_actions {
        match action {
            ServiceAction::Create(d) if !d.hosts.is_empty() => {
                host_links.push((d.name.clone(), d.hosts.clone()));
            }
            ServiceAction::Update {
                current, desired, ..
            } => {
                let (to_link, to_unlink) = host_link_unlink(desired, current);
                if !to_unlink.is_empty() {
                    host_unlinks.push((current.id, to_unlink));
                }
                if !to_link.is_empty() {
                    host_links.push((desired.name.clone(), to_link));
                }
            }
            ServiceAction::Recreate { desired, .. } if !desired.hosts.is_empty() => {
                host_links.push((desired.name.clone(), desired.hosts.clone()));
            }
            _ => {}
        }
    }

    // Reachability summary data: each acted-on service's name + desired custom
    // hosts. Base host is derived from the env slug once it's known below.
    let reachable_services: Vec<(String, Vec<String>)> = plan
        .service_actions
        .iter()
        .filter_map(|a| match a {
            ServiceAction::Create(d) => Some((d.name.clone(), d.hosts.clone())),
            ServiceAction::Update { desired, .. } | ServiceAction::Recreate { desired, .. } => {
                Some((desired.name.clone(), desired.hosts.clone()))
            }
            ServiceAction::Delete(_) => None,
        })
        .collect();

    // Standalone instances to tear down (destroy only; empty for up). Captured
    // before the plan's other fields are consumed by partitioning below.
    let instance_stops = plan.instance_stops;

    // ── Phase 1: env ──
    let (env_id, env_slug) = match plan.env_action {
        EnvAction::Use(env) => (env.id, env.slug),
        EnvAction::Create(req) => {
            let step = progress.step(
                Icon::Environment,
                &format!("Creating environment {}", req.name),
            );
            let env = client
                .create_environment(req.clone())
                .await
                .with_context(|| format!("failed to create environment {:?}", req.name))?;
            step.finish(Tone::Add, &format!("environment {} created", env.name));
            (env.id, env.slug)
        }
    };

    // service_ids: name → id, mutated as services are created/recreated.
    let mut service_ids: BTreeMap<String, Uuid> = plan.existing_service_ids.clone();

    let services = PartitionedServices::from_actions(plan.service_actions);
    let mut deployments = PartitionedDeployments::from_actions(plan.deployment_actions);

    // ── Phase 2: create new services ──
    for desired in services.creates {
        let step = progress.step(Icon::Service, &format!("Creating service {}", desired.name));
        let id = create_service(client, env_id, &desired).await?;
        service_ids.insert(desired.name.clone(), id);
        step.finish(Tone::Add, &format!("service {} created", desired.name));
    }

    // ── Phase 3: update services (config only; skip when config is unchanged
    //    and only the host set differs — hosts are reconciled by link/unlink) ──
    for (id, desired, current) in services.updates {
        if desired.configuration != current.configuration {
            let step = progress.step(Icon::Service, &format!("Updating service {}", desired.name));
            client
                .update_service(env_id, id, desired.configuration.clone())
                .await
                .with_context(|| format!("failed to update service {:?}", desired.name))?;
            step.finish(Tone::Change, &format!("service {} updated", desired.name));
        }
    }

    // ── Unlink pass: free hosts no longer desired before anything rebinds ──
    for (service_id, to_unlink) in &host_unlinks {
        for host in to_unlink {
            let step = progress.step(Icon::Host, &format!("Unlinking host {host}"));
            // An unlink target with no claimed-host row (host deleted out-of-band)
            // is already effectively unbound — skip it rather than abort the apply
            // mid-flight. Only links require a resolvable id (preflight guarantees
            // referenced hosts are claimed).
            let Some(host_id) = host_ids.get(&normalize_host(host)).copied() else {
                step.finish(
                    Tone::Warn,
                    &format!("skipping unlink of {host}: no claimed host found (already removed?)"),
                );
                continue;
            };
            client
                .unlink_host_from_service(host_id, *service_id)
                .await
                .with_context(|| format!("failed to unlink host {host:?}"))?;
            step.finish(Tone::Remove, &format!("host {host} unlinked"));
        }
    }

    // ── Phase 4: delete deployments being removed or recreated ──
    for (id, name) in deployments.ids_to_delete() {
        let step = progress.step(Icon::Deployment, &format!("Deleting deployment {name}"));
        client
            .delete_deployment(env_id, id)
            .await
            .with_context(|| format!("failed to delete deployment {name:?}"))?;
        step.finish(Tone::Remove, &format!("deployment {name} deleted"));
    }

    // ── Phase 5: recreate services (delete then create) ──
    for (current, desired) in services.recreates {
        let step = progress.step(
            Icon::Service,
            &format!("Recreating service {}", desired.name),
        );
        client
            .delete_service(env_id, current.id)
            .await
            .with_context(|| format!("failed to delete service {:?}", current.name))?;
        let new_id = create_service(client, env_id, &desired).await?;
        service_ids.insert(desired.name.clone(), new_id);
        step.finish(
            Tone::Recreate,
            &format!("service {} recreated", desired.name),
        );
    }

    // ── Phase 6: create deployments (new + recreated) ──
    for desired in deployments.drain_for_create() {
        let step = progress.step(
            Icon::Deployment,
            &format!("Creating deployment {}", desired.name),
        );
        create_deployment(client, env_id, &desired, &service_ids).await?;
        step.finish(Tone::Add, &format!("deployment {} created", desired.name));
    }

    // ── Phase 7: update deployments ──
    for (id, desired) in deployments.updates {
        let step = progress.step(
            Icon::Deployment,
            &format!("Updating deployment {}", desired.name),
        );
        client
            .update_deployment(
                env_id,
                id,
                UpdateDeploymentRequest {
                    configuration: desired.configuration.clone(),
                },
            )
            .await
            .with_context(|| format!("failed to update deployment {:?}", desired.name))?;
        step.finish(
            Tone::Change,
            &format!("deployment {} updated", desired.name),
        );
    }

    // ── Phase 8: delete services being removed ──
    //
    // ORDERING INVARIANT: deletes MUST run before the link pass. A deleted
    // service's host bindings are freed by `ON DELETE SET NULL`, so deleting
    // here (before linking) lets a host move off a removed service onto a new
    // one. Linking first would 409 ("already assigned to another service")
    // because the host is still bound to the not-yet-deleted service. More
    // broadly: every host-freeing step (this delete + the unlink pass) precedes
    // every host-binding step (the link pass below). Do not reorder.
    for current in services.deletes {
        let step = progress.step(Icon::Service, &format!("Deleting service {}", current.name));
        client
            .delete_service(env_id, current.id)
            .await
            .with_context(|| format!("failed to delete service {:?}", current.name))?;
        step.finish(Tone::Remove, &format!("service {} deleted", current.name));
    }

    // ── Link pass: bind desired hosts to their (now final-id) services ──
    for (service_name, to_link) in &host_links {
        let service_id = *service_ids.get(service_name).ok_or_else(|| {
            anyhow::anyhow!("internal: service {service_name:?} not found in id map for host link")
        })?;
        for host in to_link {
            let host_id = resolve_host_id(&host_ids, host)?;
            let step = progress.step(Icon::Host, &format!("Linking host {host}"));
            client
                .link_host_to_service(host_id, service_id)
                .await
                .with_context(|| format!("failed to link host {host:?}"))?;
            step.finish(Tone::Add, &format!("host {host} linked"));
        }
    }

    // ── Stop pass: deprovision standalone instances (destroy only) ──
    //
    // Runs after every service/deployment delete so nothing rebinds to these
    // instances mid-teardown. Deprovision is synchronous server-side, so by the
    // time these return the instances are terminal — no polling needed here.
    // `None` request = graceful shutdown with the server default timeout.
    for stop in &instance_stops {
        let name = stop.name.as_deref().unwrap_or("<unnamed>");
        let step = progress.step(Icon::Instance, &format!("Stopping instance {name}"));
        client
            .deprovision_instance(env_id, stop.id, None)
            .await
            .with_context(|| format!("failed to stop instance {name}"))?;
        step.finish(Tone::Remove, &format!("instance {name} stopped"));
    }

    // Reachability summary: every acted-on service's live base host + customs.
    let reachability: Vec<Reachability> = reachable_services
        .into_iter()
        .map(|(service, custom_hosts)| Reachability {
            base_host: format!("{service}-{env_slug}.unisrv.dev"),
            service,
            custom_hosts,
        })
        .collect();
    print!("{}", render_reachability(&reachability));

    Ok(())
}

/// Resolve a host string to its claimed-host id. Preflight guarantees every
/// referenced host is claimed, so a miss is an internal inconsistency.
fn resolve_host_id(host_ids: &BTreeMap<String, Uuid>, host: &str) -> Result<Uuid> {
    host_ids.get(&normalize_host(host)).copied().ok_or_else(|| {
        anyhow::anyhow!(
            "internal: host {host:?} is not claimed (no id); preflight should have ensured this"
        )
    })
}

/// Service actions grouped by lifecycle phase.
///
/// Field order mirrors apply order so a top-to-bottom read of the struct
/// matches the runbook in `apply()`.
#[derive(Default)]
struct PartitionedServices {
    creates: Vec<DesiredService>,
    updates: Vec<(Uuid, DesiredService, CurrentService)>,
    recreates: Vec<(CurrentService, DesiredService)>,
    deletes: Vec<CurrentService>,
}

impl PartitionedServices {
    fn from_actions(actions: Vec<ServiceAction>) -> Self {
        let mut p = Self::default();
        for action in actions {
            match action {
                ServiceAction::Create(d) => p.creates.push(d),
                ServiceAction::Update {
                    id,
                    desired,
                    current,
                } => p.updates.push((id, desired, current)),
                ServiceAction::Recreate {
                    current, desired, ..
                } => p.recreates.push((current, desired)),
                ServiceAction::Delete(c) => p.deletes.push(c),
            }
        }
        p
    }
}

/// Deployment actions grouped by lifecycle phase.
#[derive(Default)]
struct PartitionedDeployments {
    creates: Vec<DesiredDeployment>,
    updates: Vec<(Uuid, DesiredDeployment)>,
    recreates: Vec<(CurrentDeployment, DesiredDeployment)>,
    deletes: Vec<CurrentDeployment>,
}

impl PartitionedDeployments {
    fn from_actions(actions: Vec<DeploymentAction>) -> Self {
        let mut p = Self::default();
        for action in actions {
            match action {
                DeploymentAction::Create(d) => p.creates.push(d),
                DeploymentAction::Update { id, desired, .. } => p.updates.push((id, desired)),
                DeploymentAction::Recreate {
                    current, desired, ..
                } => p.recreates.push((current, desired)),
                DeploymentAction::Delete(c) => p.deletes.push(c),
            }
        }
        p
    }

    /// Phase 4 victims: explicit deletes plus the *current* half of each
    /// recreate (recreate = delete-then-create, the delete uses the old id).
    fn ids_to_delete(&self) -> Vec<(Uuid, String)> {
        self.deletes
            .iter()
            .map(|d| (d.id, d.name.clone()))
            .chain(self.recreates.iter().map(|(c, _)| (c.id, c.name.clone())))
            .collect()
    }

    /// Phase 6 work: explicit creates plus the *desired* half of each recreate.
    /// Drains the relevant fields, leaving `updates` and `deletes` intact for
    /// later phases.
    fn drain_for_create(&mut self) -> Vec<DesiredDeployment> {
        std::mem::take(&mut self.creates)
            .into_iter()
            .chain(
                std::mem::take(&mut self.recreates)
                    .into_iter()
                    .map(|(_, d)| d),
            )
            .collect()
    }
}

async fn create_service(
    client: &dyn ApiClient,
    env_id: Uuid,
    desired: &DesiredService,
) -> Result<Uuid> {
    let req = ServiceProvisionRequest {
        region: desired.region.clone(),
        name: desired.name.clone(),
        configuration: desired.configuration.clone(),
        instance_targets: vec![],
    };
    let resp = client
        .provision_service(env_id, req)
        .await
        .with_context(|| format!("failed to create service {:?}", desired.name))?;
    Ok(resp.service_id)
}

async fn create_deployment(
    client: &dyn ApiClient,
    env_id: Uuid,
    desired: &DesiredDeployment,
    service_ids: &BTreeMap<String, Uuid>,
) -> Result<()> {
    let service = match &desired.service_binding {
        Some(b) => Some(resolve_binding(b, service_ids)?),
        None => None,
    };
    let req = CreateDeploymentRequest {
        name: desired.name.clone(),
        service,
        configuration: desired.configuration.clone(),
    };
    client
        .create_deployment(env_id, req)
        .await
        .with_context(|| format!("failed to create deployment {:?}", desired.name))?;
    Ok(())
}

fn resolve_binding(
    binding: &DesiredServiceBinding,
    service_ids: &BTreeMap<String, Uuid>,
) -> Result<DeploymentServiceBinding> {
    let id = service_ids
        .get(&binding.service_name)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "internal: service {:?} not found in id map (missing or not yet created)",
                binding.service_name
            )
        })?;
    Ok(DeploymentServiceBinding {
        service_id: id,
        target_group: binding.target_group.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::up::plan::{
        CurrentDeployment, CurrentService, CurrentServiceBinding, EnvAction, InstanceStop, Plan,
        RecreateReason, ServiceAction,
    };
    use crate::progress::SilentProgress;
    use chrono::NaiveDateTime;
    use unisrv_api::models::{
        CreateDeploymentResponse, DeploymentConfiguration, EnvironmentResponse, HTTPLocation,
        HTTPLocationTarget, HTTPServiceConfig, HostResponse, ServiceProvisionResponse,
    };
    use unisrv_api::test_support::MockApiClient;

    use crate::commands::up::plan::ResolvedEnvironment;

    /// Minimal claimed-host fixture for host→id resolution in apply.
    fn host_response(id: Uuid, host: &str) -> HostResponse {
        HostResponse {
            id,
            host: host.into(),
            user_id: Uuid::new_v4(),
            service_id: None,
            certificate_type: None,
            certificate_valid_until: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
        }
    }

    #[tokio::test]
    async fn create_links_desired_hosts_to_new_service() {
        // A newly created service links all its desired hosts, bound to the id
        // returned by the create (resolved via the service_ids map).
        let new_svc_id = Uuid::new_v4();
        let h_id = Uuid::new_v4();
        let hosts = vec![host_response(h_id, "shop.acme.com")];

        let client = MockApiClient::logged_in()
            .push_provision_service(Ok(ServiceProvisionResponse {
                service_id: new_svc_id,
            }))
            .push_link_host(Ok(host_response(h_id, "shop.acme.com")));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Create(DesiredService {
                name: "web".into(),
                hosts: vec!["shop.acme.com".into()],
                region: "dev".into(),
                configuration: http_config(),
            })],
            deployment_actions: vec![],
            existing_service_ids: BTreeMap::new(),
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.link_host_calls, vec![(h_id, new_svc_id)]);
        assert!(calls.unlink_host_calls.is_empty());
    }

    #[tokio::test]
    async fn recreate_relinks_all_desired_hosts_to_new_service_id() {
        // A recreate deletes the old service (cascade-nulling its host links),
        // so all desired hosts must be re-linked to the NEW service id.
        let old_id = Uuid::new_v4();
        let new_id = Uuid::new_v4();
        let h_id = Uuid::new_v4();
        let hosts = vec![host_response(h_id, "shop.acme.com")];

        let client = MockApiClient::logged_in()
            .push_provision_service(Ok(ServiceProvisionResponse { service_id: new_id }))
            .push_link_host(Ok(host_response(h_id, "shop.acme.com")));

        let mut existing = BTreeMap::new();
        existing.insert("web".to_string(), old_id);

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Recreate {
                current: CurrentService {
                    id: old_id,
                    name: "web".into(),
                    hosts: vec!["shop.acme.com".into()],
                    region: "dev".into(),
                    configuration: http_config(),
                },
                desired: DesiredService {
                    name: "web".into(),
                    hosts: vec!["shop.acme.com".into()],
                    region: "us-east".into(),
                    configuration: http_config(),
                },
                reasons: vec![RecreateReason::ImmutableField {
                    field: "region",
                    old: "dev".into(),
                    new: "us-east".into(),
                }],
            }],
            deployment_actions: vec![],
            existing_service_ids: existing,
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        // No explicit unlink (cascade handles the old service's links).
        assert!(calls.unlink_host_calls.is_empty());
        // Re-linked to the NEW service id.
        assert_eq!(calls.link_host_calls, vec![(h_id, new_id)]);
    }

    #[tokio::test]
    async fn host_only_update_links_added_unlinks_removed_no_config_put() {
        // Service config is identical; only its host set changes (c.com → b.com).
        // Apply must unlink c.com and link b.com (unlink BEFORE link), resolve
        // both host UUIDs from the hosts list, and NOT re-PUT the config.
        let svc_id = Uuid::new_v4();
        let b_id = Uuid::new_v4();
        let c_id = Uuid::new_v4();
        let hosts = vec![host_response(b_id, "b.com"), host_response(c_id, "c.com")];

        let client = MockApiClient::logged_in()
            .push_unlink_host(Ok(host_response(c_id, "c.com")))
            .push_link_host(Ok(host_response(b_id, "b.com")));

        let mut existing = BTreeMap::new();
        existing.insert("web".to_string(), svc_id);

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Update {
                id: svc_id,
                desired: DesiredService {
                    name: "web".into(),
                    hosts: vec!["b.com".into()],
                    region: "dev".into(),
                    configuration: http_config(),
                },
                current: CurrentService {
                    id: svc_id,
                    name: "web".into(),
                    hosts: vec!["c.com".into()],
                    region: "dev".into(),
                    configuration: http_config(),
                },
            }],
            deployment_actions: vec![],
            existing_service_ids: existing,
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.unlink_host_calls, vec![(c_id, svc_id)]);
        assert_eq!(calls.link_host_calls, vec![(b_id, svc_id)]);
        assert!(
            calls.update_service_calls.is_empty(),
            "config unchanged → no update_service PUT"
        );
        let unlink_pos = calls
            .call_order
            .iter()
            .position(|m| *m == "unlink_host_from_service");
        let link_pos = calls
            .call_order
            .iter()
            .position(|m| *m == "link_host_to_service");
        assert!(unlink_pos < link_pos, "unlink must precede link");
    }

    #[tokio::test]
    async fn deleted_service_is_removed_before_host_is_linked_elsewhere() {
        // A host moves off a service being DELETED onto a new service. The delete
        // (which cascade-frees the host via ON DELETE SET NULL) must run BEFORE
        // the link, or the backend 409s ("already assigned to another service").
        let web_id = Uuid::new_v4();
        let unisrv_id = Uuid::new_v4();
        let h_id = Uuid::new_v4();
        let mut h = host_response(h_id, "h.example");
        h.service_id = Some(web_id); // currently bound to the doomed service
        let hosts = vec![h];

        let client = MockApiClient::logged_in()
            .push_provision_service(Ok(ServiceProvisionResponse {
                service_id: unisrv_id,
            }))
            .push_delete_service(Ok(()))
            .push_link_host(Ok(host_response(h_id, "h.example")));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![
                ServiceAction::Create(DesiredService {
                    name: "unisrv".into(),
                    hosts: vec!["h.example".into()],
                    region: "dev".into(),
                    configuration: http_config(),
                }),
                ServiceAction::Delete(CurrentService {
                    id: web_id,
                    name: "web".into(),
                    hosts: vec!["h.example".into()],
                    region: "dev".into(),
                    configuration: http_config(),
                }),
            ],
            deployment_actions: vec![],
            existing_service_ids: BTreeMap::new(),
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        let delete_pos = calls.call_order.iter().position(|m| *m == "delete_service");
        let link_pos = calls
            .call_order
            .iter()
            .position(|m| *m == "link_host_to_service");
        assert!(
            delete_pos < link_pos,
            "delete_service must precede link_host_to_service, got order: {:?}",
            calls.call_order
        );
    }

    #[tokio::test]
    async fn unresolvable_unlink_target_is_skipped_not_fatal() {
        // A surviving service drops a host that is no longer in the claimed-host
        // list (e.g. the host row was deleted out-of-band). The unlink can't be
        // resolved to an id; apply must skip it (the binding is moot) rather than
        // abort mid-flight and leave a partially-applied environment.
        let svc_id = Uuid::new_v4();
        let hosts: Vec<HostResponse> = vec![]; // "gone.example" not present
        let client = MockApiClient::logged_in(); // no unlink response queued

        let mut existing = BTreeMap::new();
        existing.insert("web".to_string(), svc_id);
        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Update {
                id: svc_id,
                desired: DesiredService {
                    name: "web".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: http_config(),
                },
                current: CurrentService {
                    id: svc_id,
                    name: "web".into(),
                    hosts: vec!["gone.example".into()],
                    region: "dev".into(),
                    configuration: http_config(),
                },
            }],
            deployment_actions: vec![],
            existing_service_ids: existing,
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &SilentProgress).await.unwrap(); // must NOT error
        assert!(client.calls.lock().unwrap().unlink_host_calls.is_empty());
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

    fn dep_config(image: &str) -> DeploymentConfiguration {
        DeploymentConfiguration {
            replicas: 1,
            region: "dev".into(),
            container_image: image.into(),
            args: None,
            env: None,
            vcpu_ratio: 0.25,
            vcpu_count: 1,
            memory_mb: 256,
            network: None,
            instance_port: Some(80),
        }
    }

    fn use_env() -> EnvAction {
        EnvAction::Use(ResolvedEnvironment {
            id: Uuid::new_v4(),
            name: "prod".into(),
            project: "demo".into(),
            slug: "ab12".into(),
        })
    }

    #[tokio::test]
    async fn applies_create_env_then_service_then_deployment() {
        let svc_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let new_env_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_create_environment(Ok(EnvironmentResponse {
                id: new_env_id,
                project: "demo".into(),
                name: "prod".into(),
                slug: "ab12".into(),
                display_name: None,
                description: None,
                created_at: NaiveDateTime::default(),
                updated_at: NaiveDateTime::default(),
            }))
            .push_provision_service(Ok(ServiceProvisionResponse { service_id: svc_id }))
            .push_create_deployment(Ok(CreateDeploymentResponse { id: dep_id }));

        let plan = Plan {
            project: "demo".into(),
            env_action: EnvAction::Create(unisrv_api::models::CreateEnvironmentRequest {
                project: "demo".into(),
                name: "prod".into(),
                display_name: None,
                description: None,
            }),
            service_actions: vec![ServiceAction::Create(DesiredService {
                name: "web".into(),
                hosts: vec![],
                region: "dev".into(),
                configuration: http_config(),
            })],
            deployment_actions: vec![DeploymentAction::Create(DesiredDeployment {
                name: "web".into(),
                configuration: dep_config("nginx:1"),
                service_binding: Some(DesiredServiceBinding {
                    service_name: "web".into(),
                    target_group: "default".into(),
                }),
            })],
            existing_service_ids: BTreeMap::new(),
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.create_environment_calls.len(), 1);
        assert_eq!(calls.provision_service_calls.len(), 1);
        let (env_for_service, service_req) = &calls.provision_service_calls[0];
        assert_eq!(*env_for_service, new_env_id);
        assert_eq!(service_req.name, "web");

        assert_eq!(calls.create_deployment_calls.len(), 1);
        let (env_for_dep, dep_req) = &calls.create_deployment_calls[0];
        assert_eq!(*env_for_dep, new_env_id);
        let binding = dep_req.service.as_ref().unwrap();
        assert_eq!(binding.service_id, svc_id);
        assert_eq!(binding.target_group, "default");
    }

    #[tokio::test]
    async fn stops_standalone_instances_after_service_deletes() {
        // destroy appends instance_stops to the plan; apply must deprovision each
        // one, and only after the service/deployment deletes (so nothing rebinds).
        let inst_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_delete_service(Ok(()))
            .push_deprovision_instance(Ok(()));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Delete(CurrentService {
                id: Uuid::new_v4(),
                name: "web".into(),
                hosts: vec![],
                region: "dev".into(),
                configuration: http_config(),
            })],
            deployment_actions: vec![],
            existing_service_ids: BTreeMap::new(),
            instance_stops: vec![InstanceStop {
                id: inst_id,
                name: Some("worker-0".into()),
            }],
        };

        apply(plan, &client, &[], &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.deprovision_instance_calls.len(), 1);
        let (_, stopped_id, req) = &calls.deprovision_instance_calls[0];
        assert_eq!(*stopped_id, inst_id);
        assert!(req.is_none(), "destroy uses a graceful default shutdown");

        let order = &calls.call_order;
        let svc = order.iter().rposition(|m| *m == "delete_service").unwrap();
        let stop = order
            .iter()
            .position(|m| *m == "deprovision_instance")
            .unwrap();
        assert!(
            svc < stop,
            "instance stop must follow service delete: {order:?}"
        );
    }

    #[tokio::test]
    async fn up_with_no_instance_stops_makes_zero_deprovision_calls() {
        // up never populates instance_stops, so apply must not touch instances.
        let client = MockApiClient::logged_in().push_update_service(Ok(()));
        let svc_id = Uuid::new_v4();
        let mut existing = BTreeMap::new();
        existing.insert("web".to_string(), svc_id);
        let mut new_cfg = http_config();
        new_cfg.allow_http = true;

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Update {
                id: svc_id,
                desired: DesiredService {
                    name: "web".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: new_cfg,
                },
                current: CurrentService {
                    id: svc_id,
                    name: "web".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: http_config(),
                },
            }],
            deployment_actions: vec![],
            existing_service_ids: existing,
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &SilentProgress).await.unwrap();

        assert!(
            client
                .calls
                .lock()
                .unwrap()
                .deprovision_instance_calls
                .is_empty()
        );
    }

    #[tokio::test]
    async fn applies_update_only() {
        let svc_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_update_service(Ok(()))
            .push_update_deployment(Ok(()));

        let mut existing = BTreeMap::new();
        existing.insert("web".to_string(), svc_id);

        let mut new_cfg = http_config();
        new_cfg.allow_http = true;

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Update {
                id: svc_id,
                desired: DesiredService {
                    name: "web".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: new_cfg,
                },
                current: CurrentService {
                    id: svc_id,
                    name: "web".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: http_config(),
                },
            }],
            deployment_actions: vec![DeploymentAction::Update {
                id: dep_id,
                desired: DesiredDeployment {
                    name: "web".into(),
                    configuration: dep_config("nginx:2"),
                    service_binding: None,
                },
                current: CurrentDeployment {
                    id: dep_id,
                    name: "web".into(),
                    configuration: dep_config("nginx:1"),
                    service_binding: None,
                },
            }],
            existing_service_ids: existing,
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.update_service_calls.len(), 1);
        assert_eq!(calls.update_deployment_calls.len(), 1);
        assert_eq!(calls.provision_service_calls.len(), 0);
        assert_eq!(calls.create_deployment_calls.len(), 0);
    }

    #[tokio::test]
    async fn service_recreate_uses_new_id_for_dependent_deployment() {
        let old_svc_id = Uuid::new_v4();
        let new_svc_id = Uuid::new_v4();
        let old_dep_id = Uuid::new_v4();
        let new_dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_provision_service(Ok(ServiceProvisionResponse {
                service_id: new_svc_id,
            }))
            .push_create_deployment(Ok(CreateDeploymentResponse { id: new_dep_id }));

        let mut existing = BTreeMap::new();
        existing.insert("web".to_string(), old_svc_id);

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Recreate {
                current: CurrentService {
                    id: old_svc_id,
                    name: "web".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: http_config(),
                },
                desired: DesiredService {
                    name: "web".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: http_config(),
                },
                reasons: vec![RecreateReason::ImmutableField {
                    field: "host",
                    old: "old.example".into(),
                    new: "new.example".into(),
                }],
            }],
            deployment_actions: vec![DeploymentAction::Recreate {
                current: CurrentDeployment {
                    id: old_dep_id,
                    name: "web".into(),
                    configuration: dep_config("nginx:1"),
                    service_binding: Some(CurrentServiceBinding {
                        service_id: old_svc_id,
                        service_name: "web".into(),
                        target_group: "default".into(),
                    }),
                },
                desired: DesiredDeployment {
                    name: "web".into(),
                    configuration: dep_config("nginx:1"),
                    service_binding: Some(DesiredServiceBinding {
                        service_name: "web".into(),
                        target_group: "default".into(),
                    }),
                },
                reasons: vec![RecreateReason::DependentServiceRecreated {
                    service_name: "web".into(),
                }],
            }],
            existing_service_ids: existing,
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        // Old deployment deleted before service recreate.
        assert_eq!(calls.delete_deployment_calls.len(), 1);
        assert_eq!(calls.delete_deployment_calls[0].1, old_dep_id);
        // Old service deleted, new one provisioned.
        assert_eq!(calls.delete_service_calls.len(), 1);
        assert_eq!(calls.delete_service_calls[0].1, old_svc_id);
        assert_eq!(calls.provision_service_calls.len(), 1);
        // New deployment binds to NEW service ID.
        assert_eq!(calls.create_deployment_calls.len(), 1);
        let (_env, req) = &calls.create_deployment_calls[0];
        assert_eq!(req.service.as_ref().unwrap().service_id, new_svc_id);
    }

    #[tokio::test]
    async fn delete_service_runs_after_deployments_removed() {
        let svc_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_delete_deployment(Ok(()))
            .push_delete_service(Ok(()));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Delete(CurrentService {
                id: svc_id,
                name: "old".into(),
                hosts: vec![],
                region: "dev".into(),
                configuration: http_config(),
            })],
            deployment_actions: vec![DeploymentAction::Delete(CurrentDeployment {
                id: dep_id,
                name: "old".into(),
                configuration: dep_config("img:1"),
                service_binding: None,
            })],
            existing_service_ids: BTreeMap::new(),
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.delete_deployment_calls.len(), 1);
        assert_eq!(calls.delete_service_calls.len(), 1);
    }

    #[tokio::test]
    async fn deployment_create_without_binding_works() {
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_create_deployment(Ok(CreateDeploymentResponse { id: dep_id }));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Create(DesiredDeployment {
                name: "worker".into(),
                configuration: dep_config("w:1"),
                service_binding: None,
            })],
            existing_service_ids: BTreeMap::new(),
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();
        let (_env, req) = &calls.create_deployment_calls[0];
        assert!(req.service.is_none());
    }

    /// Drives every variant of `ServiceAction` and `DeploymentAction` through
    /// `apply()` in a single run. Verifies (a) that each action issues the
    /// expected API call and (b) the documented phase ordering: creates →
    /// updates → deployment-deletes → service-recreate → deployment-creates
    /// → deployment-updates → service-deletes.
    #[tokio::test]
    async fn applies_kitchen_sink_in_correct_phase_order() {
        let stable_svc_id = Uuid::new_v4();
        let update_svc_id = Uuid::new_v4();
        let old_recreate_svc_id = Uuid::new_v4();
        let new_recreate_svc_id = Uuid::new_v4();
        let new_create_svc_id = Uuid::new_v4();
        let delete_svc_id = Uuid::new_v4();
        let update_dep_id = Uuid::new_v4();
        let old_recreate_dep_id = Uuid::new_v4();
        let delete_dep_id = Uuid::new_v4();
        let new_create_dep_id = Uuid::new_v4();
        let new_recreate_dep_id = Uuid::new_v4();

        // Two provision_service calls: phase 2 for create-svc, phase 5 for
        // recreate-svc. FIFO order, so push create-svc first.
        let client = MockApiClient::logged_in()
            .push_provision_service(Ok(ServiceProvisionResponse {
                service_id: new_create_svc_id,
            }))
            .push_provision_service(Ok(ServiceProvisionResponse {
                service_id: new_recreate_svc_id,
            }))
            // Two create_deployment calls in phase 6: create-dep then recreate-dep.
            .push_create_deployment(Ok(CreateDeploymentResponse {
                id: new_create_dep_id,
            }))
            .push_create_deployment(Ok(CreateDeploymentResponse {
                id: new_recreate_dep_id,
            }));

        let mut existing = BTreeMap::new();
        existing.insert("stable-svc".to_string(), stable_svc_id);
        existing.insert("update-svc".to_string(), update_svc_id);
        existing.insert("recreate-svc".to_string(), old_recreate_svc_id);
        existing.insert("delete-svc".to_string(), delete_svc_id);

        let mut updated_cfg = http_config();
        updated_cfg.allow_http = true;

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![
                ServiceAction::Create(DesiredService {
                    name: "create-svc".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: http_config(),
                }),
                ServiceAction::Update {
                    id: update_svc_id,
                    desired: DesiredService {
                        name: "update-svc".into(),
                        hosts: vec![],
                        region: "dev".into(),
                        configuration: updated_cfg,
                    },
                    current: CurrentService {
                        id: update_svc_id,
                        name: "update-svc".into(),
                        hosts: vec![],
                        region: "dev".into(),
                        configuration: http_config(),
                    },
                },
                ServiceAction::Recreate {
                    current: CurrentService {
                        id: old_recreate_svc_id,
                        name: "recreate-svc".into(),
                        hosts: vec![],
                        region: "dev".into(),
                        configuration: http_config(),
                    },
                    desired: DesiredService {
                        name: "recreate-svc".into(),
                        hosts: vec![],
                        region: "dev".into(),
                        configuration: http_config(),
                    },
                    reasons: vec![RecreateReason::ImmutableField {
                        field: "host",
                        old: "old-recreate.example".into(),
                        new: "new-recreate.example".into(),
                    }],
                },
                ServiceAction::Delete(CurrentService {
                    id: delete_svc_id,
                    name: "delete-svc".into(),
                    hosts: vec![],
                    region: "dev".into(),
                    configuration: http_config(),
                }),
            ],
            deployment_actions: vec![
                DeploymentAction::Create(DesiredDeployment {
                    name: "create-dep".into(),
                    configuration: dep_config("nginx:new"),
                    // Binds to the just-created create-svc to exercise the
                    // service_ids handoff between phases 2 and 6.
                    service_binding: Some(DesiredServiceBinding {
                        service_name: "create-svc".into(),
                        target_group: "default".into(),
                    }),
                }),
                DeploymentAction::Update {
                    id: update_dep_id,
                    desired: DesiredDeployment {
                        name: "update-dep".into(),
                        configuration: dep_config("nginx:2"),
                        service_binding: Some(DesiredServiceBinding {
                            service_name: "stable-svc".into(),
                            target_group: "default".into(),
                        }),
                    },
                    current: CurrentDeployment {
                        id: update_dep_id,
                        name: "update-dep".into(),
                        configuration: dep_config("nginx:1"),
                        service_binding: Some(CurrentServiceBinding {
                            service_id: stable_svc_id,
                            service_name: "stable-svc".into(),
                            target_group: "default".into(),
                        }),
                    },
                },
                DeploymentAction::Recreate {
                    current: CurrentDeployment {
                        id: old_recreate_dep_id,
                        name: "recreate-dep".into(),
                        configuration: dep_config("nginx:1"),
                        service_binding: Some(CurrentServiceBinding {
                            service_id: stable_svc_id,
                            service_name: "stable-svc".into(),
                            target_group: "default".into(),
                        }),
                    },
                    desired: DesiredDeployment {
                        name: "recreate-dep".into(),
                        configuration: dep_config("nginx:1"),
                        service_binding: Some(DesiredServiceBinding {
                            service_name: "create-svc".into(),
                            target_group: "default".into(),
                        }),
                    },
                    reasons: vec![RecreateReason::ServiceBindingChanged],
                },
                DeploymentAction::Delete(CurrentDeployment {
                    id: delete_dep_id,
                    name: "delete-dep".into(),
                    configuration: dep_config("delete:1"),
                    service_binding: None,
                }),
            ],
            existing_service_ids: existing,
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &SilentProgress).await.unwrap();

        let calls = client.calls.lock().unwrap();

        // ── Each action ran exactly the expected API calls ──
        // env was Use, no create_environment.
        assert_eq!(calls.create_environment_calls.len(), 0);
        // Two provision calls: create-svc (phase 2) and recreate-svc (phase 5).
        let provisioned: Vec<&str> = calls
            .provision_service_calls
            .iter()
            .map(|(_, req)| req.name.as_str())
            .collect();
        assert_eq!(provisioned, vec!["create-svc", "recreate-svc"]);

        assert_eq!(calls.update_service_calls.len(), 1);
        assert_eq!(calls.update_service_calls[0].1, update_svc_id);

        // Phase 4 deletes: explicit delete-dep + recreate-dep's old id.
        let deleted_deps: Vec<Uuid> = calls
            .delete_deployment_calls
            .iter()
            .map(|(_, id)| *id)
            .collect();
        assert!(deleted_deps.contains(&delete_dep_id));
        assert!(deleted_deps.contains(&old_recreate_dep_id));
        assert_eq!(deleted_deps.len(), 2);

        // Phase 5 + phase 8 service deletes: recreate-svc.old, then delete-svc.
        let deleted_svcs: Vec<Uuid> = calls
            .delete_service_calls
            .iter()
            .map(|(_, id)| *id)
            .collect();
        assert_eq!(deleted_svcs, vec![old_recreate_svc_id, delete_svc_id]);

        // Phase 6 deployment creates: create-dep, then recreate-dep.
        // recreate-dep must bind to the *new* create-svc id, since it was
        // produced by phase 2.
        let dep_creates: Vec<(&str, Option<Uuid>)> = calls
            .create_deployment_calls
            .iter()
            .map(|(_, req)| {
                (
                    req.name.as_str(),
                    req.service.as_ref().map(|b| b.service_id),
                )
            })
            .collect();
        assert_eq!(
            dep_creates,
            vec![
                ("create-dep", Some(new_create_svc_id)),
                ("recreate-dep", Some(new_create_svc_id)),
            ]
        );

        // Phase 7 deployment update.
        assert_eq!(calls.update_deployment_calls.len(), 1);
        assert_eq!(calls.update_deployment_calls[0].1, update_dep_id);

        // ── Phase ordering invariants via the global call_order log ──
        let order = &calls.call_order;
        let first = |name: &str| {
            order
                .iter()
                .position(|m| *m == name)
                .unwrap_or_else(|| panic!("{name} not in call_order: {order:?}"))
        };
        let last = |name: &str| {
            order
                .iter()
                .rposition(|m| *m == name)
                .unwrap_or_else(|| panic!("{name} not in call_order: {order:?}"))
        };

        // 2 → 3: every provision_service before any update_service?
        // No — phase 2 has only one provision (create-svc), then phase 3
        // updates, then phase 5 provisions again. So: first provision_service
        // < update_service < last provision_service. That's the boundary.
        assert!(first("provision_service") < first("update_service"));
        assert!(first("update_service") < first("delete_deployment"));
        // 4 → 5: every delete_deployment before any delete_service.
        assert!(last("delete_deployment") < first("delete_service"));
        // 5 internal: recreate-svc deleted before being re-provisioned.
        assert!(first("delete_service") < last("provision_service"));
        // 5 → 6: recreate-svc provisioned before any deployment is created.
        assert!(last("provision_service") < first("create_deployment"));
        // 6 → 7: every deployment create before any deployment update.
        assert!(last("create_deployment") < first("update_deployment"));
        // 7 → 8: deployment update before final service delete (delete-svc).
        assert!(first("update_deployment") < last("delete_service"));
    }
}
