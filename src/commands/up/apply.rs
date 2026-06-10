//! Execute a [`Plan`] against the API.
//!
//! Ordering rationale (see plan.rs for backend constraints):
//! 1. Create env (if EnvAction::Create).
//! 2. Create new networks (before anything references them).
//! 3. Create new services.
//! 4. Update services (config-only).
//! 5. Unlink hosts dropped by *surviving* (updated) services.
//! 6. Delete deployments being deleted *or recreated* (frees bindings, starts
//!    the instance drain networks wait on).
//! 7. Update deployments (always carries the resolved network_id — frees
//!    networks the update detaches from).
//! 8. Recreate services: delete old, then create new (new IDs).
//! 9. Recreate networks: wait for drain, delete old, create new (new IDs).
//! 10. Create deployments (new + recreated; resolves service/network ids by name).
//! 11. Delete services being fully removed (cascade-frees their hosts).
//! 12. Link hosts to their desired services.
//! 13. Stop standalone instances (destroy only).
//! 14. Delete removed networks (after the stops that free them; drain-gated).
//!
//! HOST INVARIANT: every host-FREEING step (unlink pass #5; delete cascade #11)
//! runs before the host-LINKING step (#12). A host can only be linked while
//! unbound, so freeing must precede binding — otherwise the backend 409s. Deleted
//! services are freed by the DB cascade, not an explicit unlink. Do not reorder.
//!
//! NETWORK INVARIANT: every instance-FREEING step (deployment deletes #6,
//! deployment updates #7, instance stops #13) precedes the network
//! delete/recreate that depends on it; network creates (#2, #9) precede the
//! deployment creates/updates that bind to them. The backend rejects a network
//! delete while any non-stopped instance is attached, so deletes are gated on
//! a bounded drain wait. Do not reorder.
//!
//! No rollback. On error, return immediately. Reconcile re-run will pick up.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::time::Duration;
use unisrv_api::ApiClient;
use unisrv_api::models::{
    CreateDeploymentRequest, CreateInternalNetworkRequest, DeploymentServiceBinding, HostResponse,
    ServiceProvisionRequest, UpdateDeploymentRequest,
};
use uuid::Uuid;

use super::desired::{DesiredDeployment, DesiredNetwork, DesiredService};
use super::diff::service::host_link_unlink;
use super::plan::{
    CurrentDeployment, CurrentNetwork, CurrentService, DeploymentAction, EnvAction, NetworkAction,
    Plan, ResolvedServiceBinding, ResourceRef, ServiceAction,
};
use super::render::{Reachability, render_reachability};
use crate::commands::host::normalize_host;
use crate::progress::{Icon, Progress, Tone};

/// Abstraction over sleeping between drain polls, so tests can drive poll
/// loops without real time passing. Shared with `destroy`'s deployment drain.
#[async_trait]
pub trait Waiter {
    async fn sleep(&self, dur: Duration);
}

/// Production waiter — backed by the tokio timer.
pub struct RealWaiter;

#[async_trait]
impl Waiter for RealWaiter {
    async fn sleep(&self, dur: Duration) {
        tokio::time::sleep(dur).await;
    }
}

/// Poll cadence and ceiling while waiting for a network's instances to stop
/// before deleting it. Bounded so a stuck teardown can't hang the CLI — on
/// timeout we error with a rerun hint (apply is reconcile-on-rerun).
const NETWORK_DRAIN_POLL_INTERVAL: Duration = Duration::from_secs(1);
const NETWORK_DRAIN_MAX_ATTEMPTS: usize = 60;

/// One round of a bounded poll: the awaited condition holds, or keep waiting
/// (with a progress-line detail to show).
pub enum Poll {
    Done,
    Pending(String),
}

/// Outcome of a bounded poll. The timeout error wording is the caller's job —
/// it differs per resource. `Done` carries the number of full rounds waited,
/// for callers that report elapsed time.
#[derive(Debug, PartialEq)]
pub enum PollOutcome {
    Done { rounds: usize },
    TimedOut,
}

/// Drive `check` every `interval` until it reports [`Poll::Done`], it errors,
/// or `max_attempts` rounds pass. Progress updates render the caller's detail
/// with elapsed seconds appended; sleeping goes through the [`Waiter`] seam so
/// tests run instantly.
pub async fn poll_until(
    waiter: &dyn Waiter,
    interval: Duration,
    max_attempts: usize,
    step: &crate::progress::Step,
    mut check: impl AsyncFnMut() -> Result<Poll>,
) -> Result<PollOutcome> {
    for attempt in 0..max_attempts {
        match check().await? {
            Poll::Done => return Ok(PollOutcome::Done { rounds: attempt }),
            Poll::Pending(detail) => {
                let elapsed = attempt as u64 * interval.as_secs();
                step.update(&format!("{detail} ({elapsed}s)"));
            }
        }
        waiter.sleep(interval).await;
    }
    Ok(PollOutcome::TimedOut)
}

pub async fn apply(
    plan: Plan,
    client: &dyn ApiClient,
    hosts: &[HostResponse],
    waiter: &dyn Waiter,
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
    let mut host_links: Vec<(ResourceRef, Vec<String>)> = Vec::new();
    for action in &plan.service_actions {
        match action {
            ServiceAction::Create(d) if !d.hosts.is_empty() => {
                host_links.push((
                    ResourceRef::Pending {
                        name: d.name.clone(),
                    },
                    d.hosts.clone(),
                ));
            }
            ServiceAction::Update {
                current, desired, ..
            } => {
                let (to_link, to_unlink) = host_link_unlink(desired, current);
                if !to_unlink.is_empty() {
                    host_unlinks.push((current.id, to_unlink));
                }
                if !to_link.is_empty() {
                    host_links.push((
                        ResourceRef::Existing {
                            id: current.id,
                            name: desired.name.clone(),
                        },
                        to_link,
                    ));
                }
            }
            ServiceAction::Recreate { desired, .. } if !desired.hosts.is_empty() => {
                host_links.push((
                    ResourceRef::Pending {
                        name: desired.name.clone(),
                    },
                    desired.hosts.clone(),
                ));
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

    // Minted ids: name → uuid for resources created or recreated DURING this
    // apply. References to anything else were resolved at plan time
    // (`ResourceRef::Existing`), so these maps start empty — a `Pending` ref
    // resolved before its mint phase fails loudly.
    let mut minted_service_ids: BTreeMap<String, Uuid> = BTreeMap::new();
    let mut minted_network_ids: BTreeMap<String, Uuid> = BTreeMap::new();

    let services = PartitionedServices::from_actions(plan.service_actions);
    let mut deployments = PartitionedDeployments::from_actions(plan.deployment_actions);
    let networks = PartitionedNetworks::from_actions(plan.network_actions);

    // ── Phase 2: create new networks (before anything references them) ──
    for desired in networks.creates {
        let step = progress.step(Icon::Network, &format!("Creating network {}", desired.name));
        let id = create_network(client, env_id, &desired).await?;
        minted_network_ids.insert(desired.name.clone(), id);
        step.finish(Tone::Add, &format!("network {} created", desired.name));
    }

    // ── Phase 3: create new services ──
    for desired in services.creates {
        let step = progress.step(Icon::Service, &format!("Creating service {}", desired.name));
        let id = create_service(client, env_id, &desired).await?;
        minted_service_ids.insert(desired.name.clone(), id);
        step.finish(Tone::Add, &format!("service {} created", desired.name));
    }

    // ── Phase 4: update services (config only; skip when config is unchanged
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

    // ── Phase 5: unlink pass — free hosts no longer desired before anything rebinds ──
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

    // ── Phase 6: delete deployments being removed or recreated ──
    for (id, name) in deployments.ids_to_delete() {
        let step = progress.step(Icon::Deployment, &format!("Deleting deployment {name}"));
        client
            .delete_deployment(env_id, id)
            .await
            .with_context(|| format!("failed to delete deployment {name:?}"))?;
        step.finish(Tone::Remove, &format!("deployment {name} deleted"));
    }

    // ── Phase 7: update deployments ──
    //
    // ORDERING INVARIANT: updates run BEFORE the network recreate/delete
    // phases. An update that detaches a deployment from (or moves it off) a
    // doomed network is what lets that network's instances drain — the drain
    // polls below depend on these PUTs having landed. Updates never reference
    // a recreated network (the cascade forces those deployments onto the
    // Recreate path), so every network_id here is resolvable already.
    //
    // The backend treats an absent `network_id` as a detach, so the resolved
    // desired binding is sent on EVERY update — even config-only ones.
    for (id, desired, network) in deployments.updates.drain(..) {
        let step = progress.step(
            Icon::Deployment,
            &format!("Updating deployment {}", desired.name),
        );
        let network_id = network
            .as_ref()
            .map(|r| resolve_ref(r, &minted_network_ids))
            .transpose()?;
        client
            .update_deployment(
                env_id,
                id,
                UpdateDeploymentRequest {
                    network_id,
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

    // ── Phase 8: recreate services (delete then create) ──
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
        minted_service_ids.insert(desired.name.clone(), new_id);
        step.finish(
            Tone::Recreate,
            &format!("service {} recreated", desired.name),
        );
    }

    // ── Phase 9: recreate networks (drain → delete old → create new) ──
    //
    // The UNIQUE(environment_id, name) constraint forbids creating the new
    // network before deleting the old, and the delete is rejected while any
    // non-stopped instance is attached — so wait for the drain started by the
    // deployment deletes/updates above, bounded.
    for (current, desired) in networks.recreates {
        wait_for_network_drain(client, env_id, &current, waiter, progress).await?;
        let step = progress.step(
            Icon::Network,
            &format!("Recreating network {}", desired.name),
        );
        client
            .delete_network(env_id, current.id)
            .await
            .with_context(|| format!("failed to delete network {:?}", current.name))?;
        let new_id = create_network(client, env_id, &desired).await?;
        minted_network_ids.insert(desired.name.clone(), new_id);
        step.finish(
            Tone::Recreate,
            &format!("network {} recreated", desired.name),
        );
    }

    // ── Phase 10: create deployments (new + recreated) ──
    for (desired, service, network) in deployments.drain_for_create() {
        let step = progress.step(
            Icon::Deployment,
            &format!("Creating deployment {}", desired.name),
        );
        create_deployment(
            client,
            env_id,
            &desired,
            service.as_ref(),
            network.as_ref(),
            &minted_service_ids,
            &minted_network_ids,
        )
        .await?;
        step.finish(Tone::Add, &format!("deployment {} created", desired.name));
    }

    // ── Phase 11: delete services being removed ──
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

    // ── Phase 12: link pass — bind desired hosts to their (now final-id) services ──
    for (service_ref, to_link) in &host_links {
        let service_id = resolve_ref(service_ref, &minted_service_ids)?;
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

    // ── Phase 13: stop pass — deprovision standalone instances (destroy only) ──
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

    // ── Phase 14: delete removed networks ──
    //
    // Last on purpose: nothing later depends on these being gone, and in
    // destroy the blockers can be standalone instances that are only stopped
    // by the stop pass above. The drain wait covers instances still stopping
    // (the backend rejects the delete while any non-stopped instance is
    // attached).
    for current in networks.deletes {
        wait_for_network_drain(client, env_id, &current, waiter, progress).await?;
        let step = progress.step(Icon::Network, &format!("Deleting network {}", current.name));
        client
            .delete_network(env_id, current.id)
            .await
            .with_context(|| format!("failed to delete network {:?}", current.name))?;
        step.finish(Tone::Remove, &format!("network {} deleted", current.name));
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

/// Resolve a desired network name to its live id. Config validation guarantees
/// every referenced network is defined, and apply phases keep the map current,
/// so a miss is an internal inconsistency.
/// Resolve a plan-time reference to a live uuid. `Existing` ids were resolved
/// by the diff; `Pending` names must have been minted by an earlier apply
/// phase — a miss means a phase-ordering violation and fails loudly rather
/// than binding a stale or absent resource.
fn resolve_ref(r: &ResourceRef, minted: &BTreeMap<String, Uuid>) -> Result<Uuid> {
    match r {
        ResourceRef::Existing { id, .. } => Ok(*id),
        ResourceRef::Pending { name } => minted.get(name).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "internal: {name:?} was planned as created/recreated this run but its id has \
                 not been minted yet"
            )
        }),
    }
}

/// Wait (bounded) until `network` has no active instances attached — the exact
/// predicate the backend's delete_network guard uses, so an empty list means
/// the delete will be accepted. On timeout, classify the blockers: instances
/// owned by a deployment are still converging (operator mid-roll / teardown in
/// flight) and a rerun finishes the job; standalone instances will never be
/// stopped by this command and need explicit action.
async fn wait_for_network_drain(
    client: &dyn ApiClient,
    env_id: Uuid,
    network: &CurrentNetwork,
    waiter: &dyn Waiter,
    progress: &dyn Progress,
) -> Result<()> {
    let step = progress.step(
        Icon::Network,
        &format!("Waiting for network {} to drain", network.name),
    );
    let outcome = poll_until(
        waiter,
        NETWORK_DRAIN_POLL_INTERVAL,
        NETWORK_DRAIN_MAX_ATTEMPTS,
        &step,
        async || {
            let detail = client
                .get_network(env_id, network.id)
                .await
                .with_context(|| format!("failed to inspect network {:?}", network.name))?;
            Ok(if detail.instances.is_empty() {
                Poll::Done
            } else {
                Poll::Pending(format!(
                    "Draining network {} — {} instance(s) still attached…",
                    network.name,
                    detail.instances.len()
                ))
            })
        },
    )
    .await?;
    if let PollOutcome::Done { .. } = outcome {
        step.clear();
        return Ok(());
    }

    // Timed out: inspect the blockers so the error never lies about whether a
    // rerun helps — and if the inspection itself fails, admit that instead of
    // guessing a classification from missing data.
    let inspected: Result<Vec<String>> = async {
        let attached = client.get_network(env_id, network.id).await?.instances;
        let by_id: BTreeMap<Uuid, _> = client
            .list_instances(env_id)
            .await?
            .instances
            .into_iter()
            .map(|i| (i.id, i))
            .collect();
        Ok(super::preflight::standalone_instance_names(
            &attached, &by_id,
        ))
    }
    .await;
    let timeout_secs = NETWORK_DRAIN_MAX_ATTEMPTS as u64 * NETWORK_DRAIN_POLL_INTERVAL.as_secs();
    match inspected {
        Err(e) => anyhow::bail!(
            "timed out after {timeout_secs}s waiting for network {:?} to drain; could not \
             inspect the remaining instances ({e:#}). Rerun this command shortly.",
            network.name
        ),
        Ok(standalone) if standalone.is_empty() => anyhow::bail!(
            "timed out after {timeout_secs}s waiting for network {:?} to drain; its instances \
             are still being replaced or torn down. Rerun this command shortly to finish.",
            network.name
        ),
        Ok(standalone) => anyhow::bail!(
            "network {:?} cannot be removed: standalone instance(s) [{}] are attached to it and \
             will not be stopped by this command. Stop them first, then rerun.",
            network.name,
            standalone.join(", ")
        ),
    }
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

/// A deployment to be created, with its plan-resolved references.
type DeploymentCreate = (
    DesiredDeployment,
    Option<ResolvedServiceBinding>,
    Option<ResourceRef>,
);

/// Deployment actions grouped by lifecycle phase, carrying the references the
/// diff resolved for them.
#[derive(Default)]
struct PartitionedDeployments {
    creates: Vec<DeploymentCreate>,
    updates: Vec<(Uuid, DesiredDeployment, Option<ResourceRef>)>,
    recreates: Vec<(CurrentDeployment, DeploymentCreate)>,
    deletes: Vec<CurrentDeployment>,
}

impl PartitionedDeployments {
    fn from_actions(actions: Vec<DeploymentAction>) -> Self {
        let mut p = Self::default();
        for action in actions {
            match action {
                DeploymentAction::Create {
                    desired,
                    service,
                    network,
                } => p.creates.push((desired, service, network)),
                DeploymentAction::Update {
                    id,
                    desired,
                    network,
                    ..
                } => p.updates.push((id, desired, network)),
                DeploymentAction::Recreate {
                    current,
                    desired,
                    service,
                    network,
                    ..
                } => p.recreates.push((current, (desired, service, network))),
                DeploymentAction::Delete(c) => p.deletes.push(c),
            }
        }
        p
    }

    /// Phase 6 victims: explicit deletes plus the *current* half of each
    /// recreate (recreate = delete-then-create, the delete uses the old id).
    fn ids_to_delete(&self) -> Vec<(Uuid, String)> {
        self.deletes
            .iter()
            .map(|d| (d.id, d.name.clone()))
            .chain(self.recreates.iter().map(|(c, _)| (c.id, c.name.clone())))
            .collect()
    }

    /// Phase 10 work: explicit creates plus the *desired* half of each
    /// recreate. Drains the relevant fields, leaving `updates` and `deletes`
    /// intact for later phases.
    fn drain_for_create(&mut self) -> Vec<DeploymentCreate> {
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

/// Network actions grouped by lifecycle phase.
#[derive(Default)]
struct PartitionedNetworks {
    creates: Vec<DesiredNetwork>,
    recreates: Vec<(CurrentNetwork, DesiredNetwork)>,
    deletes: Vec<CurrentNetwork>,
}

impl PartitionedNetworks {
    fn from_actions(actions: Vec<NetworkAction>) -> Self {
        let mut p = Self::default();
        for action in actions {
            match action {
                NetworkAction::Create(d) => p.creates.push(d),
                NetworkAction::Recreate {
                    current, desired, ..
                } => p.recreates.push((current, desired)),
                NetworkAction::Delete(c) => p.deletes.push(c),
            }
        }
        p
    }
}

async fn create_network(
    client: &dyn ApiClient,
    env_id: Uuid,
    desired: &DesiredNetwork,
) -> Result<Uuid> {
    let req = CreateInternalNetworkRequest {
        name: desired.name.clone(),
        ipv4_cidr: desired.ipv4_cidr.clone(),
    };
    let resp = client
        .create_network(env_id, req)
        .await
        .with_context(|| format!("failed to create network {:?}", desired.name))?;
    Ok(resp.id)
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

#[allow(clippy::too_many_arguments)]
async fn create_deployment(
    client: &dyn ApiClient,
    env_id: Uuid,
    desired: &DesiredDeployment,
    service: Option<&ResolvedServiceBinding>,
    network: Option<&ResourceRef>,
    minted_service_ids: &BTreeMap<String, Uuid>,
    minted_network_ids: &BTreeMap<String, Uuid>,
) -> Result<()> {
    let service = service
        .map(|b| {
            Ok::<_, anyhow::Error>(DeploymentServiceBinding {
                service_id: resolve_ref(&b.service, minted_service_ids)?,
                target_group: b.target_group.clone(),
            })
        })
        .transpose()?;
    let req = CreateDeploymentRequest {
        name: desired.name.clone(),
        service,
        network_id: network
            .map(|r| resolve_ref(r, minted_network_ids))
            .transpose()?,
        configuration: desired.configuration.clone(),
    };
    client
        .create_deployment(env_id, req)
        .await
        .with_context(|| format!("failed to create deployment {:?}", desired.name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::up::desired::DesiredServiceBinding;
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

    /// Test waiter that never sleeps, so drain polls run instantly.
    struct NoSleep;

    #[async_trait::async_trait]
    impl Waiter for NoSleep {
        async fn sleep(&self, _dur: std::time::Duration) {}
    }

    /// Network fixture as the backend returns it from create/get.
    fn network_response(
        id: Uuid,
        name: &str,
        cidr: &str,
        instances: Vec<unisrv_api::models::InstanceInfo>,
    ) -> unisrv_api::models::NetworkResponse {
        unisrv_api::models::NetworkResponse {
            id,
            environment_id: Uuid::new_v4(),
            name: name.into(),
            ipv4_cidr: cidr.into(),
            created_at: NaiveDateTime::default(),
            instances,
        }
    }

    #[tokio::test]
    async fn creates_network_before_deployment_and_resolves_network_id() {
        use crate::commands::up::desired::DesiredNetwork;
        use crate::commands::up::plan::NetworkAction;

        let net_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_create_network(Ok(network_response(
                net_id,
                "internal",
                "10.0.0.0/16",
                vec![],
            )))
            .push_create_deployment(Ok(CreateDeploymentResponse { id: Uuid::new_v4() }));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Create {
                service: None,
                network: Some(ResourceRef::Pending {
                    name: "internal".into(),
                }),
                desired: DesiredDeployment {
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network: Some("internal".into()),
                },
            }],
            network_actions: vec![NetworkAction::Create(DesiredNetwork {
                name: "internal".into(),
                ipv4_cidr: "10.0.0.0/16".into(),
            })],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();
        let (_, net_req) = &calls.create_network_calls[0];
        assert_eq!(net_req.name, "internal");
        assert_eq!(net_req.ipv4_cidr, "10.0.0.0/16");

        // The deployment binds to the uuid minted by create_network.
        let (_, dep_req) = &calls.create_deployment_calls[0];
        assert_eq!(dep_req.network_id, Some(net_id));

        let order = &calls.call_order;
        let net_pos = order.iter().position(|m| *m == "create_network").unwrap();
        let dep_pos = order
            .iter()
            .position(|m| *m == "create_deployment")
            .unwrap();
        assert!(net_pos < dep_pos, "network created first: {order:?}");
    }

    #[tokio::test]
    async fn config_only_update_still_sends_unchanged_network_id() {
        // The backend treats an absent/null network_id on update as a DETACH.
        // A plain image bump on a deployment whose network binding is
        // unchanged must therefore still send the resolved id, or the update
        // would silently disconnect the deployment from its network.
        use crate::commands::up::plan::CurrentNetworkBinding;

        let net_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in().push_update_deployment(Ok(()));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Update {
                // The diff resolved the unchanged network at plan time.
                network: Some(ResourceRef::Existing {
                    id: net_id,
                    name: "internal".into(),
                }),
                id: dep_id,
                desired: DesiredDeployment {
                    name: "api".into(),
                    configuration: dep_config("i:2"),
                    service_binding: None,
                    network: Some("internal".into()),
                },
                current: CurrentDeployment {
                    id: dep_id,
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network_binding: Some(CurrentNetworkBinding {
                        network_id: net_id,
                        network_name: "internal".into(),
                    }),
                },
            }],
            network_actions: vec![],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();
        let (_, _, req) = &calls.update_deployment_calls[0];
        assert_eq!(
            req.network_id,
            Some(net_id),
            "unchanged binding must still be sent — absent means detach"
        );
    }

    #[tokio::test]
    async fn detach_update_sends_null_network_id() {
        // Removing `network` from the HCL is an in-place update with
        // network_id: None — the explicit detach.
        use crate::commands::up::plan::CurrentNetworkBinding;

        let net_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in().push_update_deployment(Ok(()));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Update {
                network: None, // detach: no desired ref at all
                id: dep_id,
                desired: DesiredDeployment {
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network: None,
                },
                current: CurrentDeployment {
                    id: dep_id,
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network_binding: Some(CurrentNetworkBinding {
                        network_id: net_id,
                        network_name: "internal".into(),
                    }),
                },
            }],
            network_actions: vec![],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();
        let (_, _, req) = &calls.update_deployment_calls[0];
        assert_eq!(req.network_id, None);
    }

    #[tokio::test]
    async fn network_recreate_waits_for_drain_then_rebinds_dependents_to_new_id() {
        // CIDR change: the dependent deployment is deleted first (cascade), the
        // old network is polled until its instances are gone, then deleted and
        // re-created under a NEW uuid, and the deployment re-created bound to it.
        use crate::commands::up::desired::DesiredNetwork;
        use crate::commands::up::plan::{CurrentNetwork, CurrentNetworkBinding, NetworkAction};
        use unisrv_api::models::InstanceInfo;

        let old_net_id = Uuid::new_v4();
        let new_net_id = Uuid::new_v4();
        let old_dep_id = Uuid::new_v4();
        let straggler = InstanceInfo {
            id: Uuid::new_v4(),
            internal_ip: "10.0.0.1".into(),
        };

        let client = MockApiClient::logged_in()
            .push_delete_deployment(Ok(()))
            // Drain poll: one instance still stopping, then clear.
            .push_get_network(Ok(network_response(
                old_net_id,
                "internal",
                "10.0.0.0/16",
                vec![straggler],
            )))
            .push_get_network(Ok(network_response(
                old_net_id,
                "internal",
                "10.0.0.0/16",
                vec![],
            )))
            .push_delete_network(Ok(()))
            .push_create_network(Ok(network_response(
                new_net_id,
                "internal",
                "10.9.0.0/24",
                vec![],
            )))
            .push_create_deployment(Ok(CreateDeploymentResponse { id: Uuid::new_v4() }));

        let mut existing_networks = BTreeMap::new();
        existing_networks.insert("internal".to_string(), old_net_id);

        let current_net = CurrentNetwork {
            id: old_net_id,
            name: "internal".into(),
            ipv4_cidr: "10.0.0.0/16".into(),
        };
        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Recreate {
                // The network is recreated this run, so the ref is Pending —
                // it must bind the NEW uuid minted in the recreate phase.
                network: Some(ResourceRef::Pending {
                    name: "internal".into(),
                }),
                service: None,
                current: CurrentDeployment {
                    id: old_dep_id,
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network_binding: Some(CurrentNetworkBinding {
                        network_id: old_net_id,
                        network_name: "internal".into(),
                    }),
                },
                desired: DesiredDeployment {
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network: Some("internal".into()),
                },
                reasons: vec![RecreateReason::DependentNetworkRecreated {
                    network_name: "internal".into(),
                }],
            }],
            network_actions: vec![NetworkAction::Recreate {
                current: current_net.clone(),
                desired: DesiredNetwork {
                    name: "internal".into(),
                    ipv4_cidr: "10.9.0.0/24".into(),
                },
                reasons: vec![RecreateReason::ImmutableField {
                    field: "iprange",
                    old: "10.0.0.0/16".into(),
                    new: "10.9.0.0/24".into(),
                }],
            }],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();
        // Drained: polled twice (instance attached, then clear).
        assert_eq!(calls.get_network_calls.len(), 2);
        assert_eq!(
            calls.delete_network_calls,
            vec![(calls.delete_network_calls[0].0, old_net_id)]
        );
        // Recreated deployment binds the NEW network id.
        let (_, dep_req) = &calls.create_deployment_calls[0];
        assert_eq!(dep_req.network_id, Some(new_net_id));

        let order = &calls.call_order;
        let pos = |n: &str| order.iter().position(|m| *m == n).unwrap();
        assert!(
            pos("delete_deployment") < pos("get_network"),
            "drain starts after the dependent deployment delete: {order:?}"
        );
        assert!(pos("get_network") < pos("delete_network"), "{order:?}");
        assert!(pos("delete_network") < pos("create_network"), "{order:?}");
        assert!(
            pos("create_network") < pos("create_deployment"),
            "{order:?}"
        );
    }

    #[tokio::test]
    async fn network_drain_timeout_with_deployment_owned_blockers_hints_rerun() {
        // Instances that belong to a deployment are converging (operator
        // mid-roll); the timeout error must say a rerun finishes the job.
        use crate::commands::up::plan::{CurrentNetwork, NetworkAction};
        use unisrv_api::models::{
            DeploymentInfo, InstanceInfo, InstanceListEntry, InstanceListResponse, InstanceState,
        };

        let net_id = Uuid::new_v4();
        let inst_id = Uuid::new_v4();
        let blocked = network_response(
            net_id,
            "internal",
            "10.0.0.0/16",
            vec![InstanceInfo {
                id: inst_id,
                internal_ip: "10.0.0.1".into(),
            }],
        );
        let mut client = MockApiClient::logged_in();
        // Never drains: every poll (plus the final classification fetch) sees
        // the same attached instance.
        for _ in 0..(super::NETWORK_DRAIN_MAX_ATTEMPTS + 1) {
            client = client.push_get_network(Ok(network_response(
                blocked.id,
                &blocked.name,
                &blocked.ipv4_cidr,
                blocked.instances.clone(),
            )));
        }
        let client = client.with_list_instances(Ok(InstanceListResponse {
            instances: vec![InstanceListEntry {
                id: inst_id,
                name: Some("api-0".into()),
                state: InstanceState("stopping".into()),
                container_image: "i:1".into(),
                created_at: NaiveDateTime::default(),
                deployment: Some(DeploymentInfo {
                    id: Uuid::new_v4(),
                    name: "api".into(),
                }),
            }],
        }));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![],
            network_actions: vec![NetworkAction::Delete(CurrentNetwork {
                id: net_id,
                name: "internal".into(),
                ipv4_cidr: "10.0.0.0/16".into(),
            })],
            instance_stops: vec![],
        };

        let err = apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("internal"), "names the network: {msg}");
        assert!(msg.contains("Rerun"), "hints rerun: {msg}");
        // The same drain runs inside `unisrv destroy` — wording must be
        // command-neutral, never "rerun `unisrv up`".
        assert!(
            !msg.contains("unisrv up"),
            "wording must be command-neutral: {msg}"
        );
    }

    #[tokio::test]
    async fn update_referencing_recreated_network_fails_loudly_not_stale() {
        // The diff's cascade guarantees no Update ever references a network
        // being recreated — but apply must not depend on a guarantee enforced
        // in another module. Given a (malformed) plan that violates it, apply
        // must fail loudly rather than silently bind the doomed old uuid.
        use crate::commands::up::desired::DesiredNetwork;
        use crate::commands::up::plan::{CurrentNetwork, NetworkAction};

        let old_net_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in();

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Update {
                // Violates the cascade: the diff would never emit an Update
                // with a Pending ref (recreated networks force Recreate). The
                // update phase runs before the network recreate mints the id,
                // so this must fail loudly — never bind the doomed old uuid.
                network: Some(ResourceRef::Pending {
                    name: "internal".into(),
                }),
                id: dep_id,
                desired: DesiredDeployment {
                    name: "api".into(),
                    configuration: dep_config("i:2"),
                    service_binding: None,
                    network: Some("internal".into()),
                },
                current: CurrentDeployment {
                    id: dep_id,
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network_binding: None,
                },
            }],
            network_actions: vec![NetworkAction::Recreate {
                current: CurrentNetwork {
                    id: old_net_id,
                    name: "internal".into(),
                    ipv4_cidr: "10.0.0.0/16".into(),
                },
                desired: DesiredNetwork {
                    name: "internal".into(),
                    ipv4_cidr: "10.9.0.0/24".into(),
                },
                reasons: vec![RecreateReason::ImmutableField {
                    field: "iprange",
                    old: "10.0.0.0/16".into(),
                    new: "10.9.0.0/24".into(),
                }],
            }],
            instance_stops: vec![],
        };

        let err = apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("internal"),
            "fails loudly naming the network: {msg}"
        );
        // The stale uuid must never have been sent.
        assert!(
            client
                .calls
                .lock()
                .unwrap()
                .update_deployment_calls
                .is_empty(),
            "must not PUT the doomed old network id"
        );
    }

    #[tokio::test]
    async fn network_drain_timeout_with_failed_classification_is_honest() {
        // If the blocker inspection itself fails after the timeout, the error
        // must say so — never guess a classification from missing data.
        use crate::commands::up::plan::{CurrentNetwork, NetworkAction};
        use unisrv_api::models::InstanceInfo;

        let net_id = Uuid::new_v4();
        let mut client = MockApiClient::logged_in();
        // Every poll sees one attached instance; the post-timeout inspection
        // call then fails.
        for _ in 0..super::NETWORK_DRAIN_MAX_ATTEMPTS {
            client = client.push_get_network(Ok(network_response(
                net_id,
                "internal",
                "10.0.0.0/16",
                vec![InstanceInfo {
                    id: Uuid::new_v4(),
                    internal_ip: "10.0.0.1".into(),
                }],
            )));
        }
        let client = client.push_get_network(Err(unisrv_api::ApiError::Server {
            status: 500,
            reason: "boom".into(),
        }));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![],
            network_actions: vec![NetworkAction::Delete(CurrentNetwork {
                id: net_id,
                name: "internal".into(),
                ipv4_cidr: "10.0.0.0/16".into(),
            })],
            instance_stops: vec![],
        };

        let err = apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("timed out"),
            "keeps the timeout framing: {msg}"
        );
        assert!(
            msg.contains("could not inspect"),
            "admits the inspection failed: {msg}"
        );
        assert!(
            !msg.contains("standalone"),
            "must not guess a classification: {msg}"
        );
    }

    #[tokio::test]
    async fn network_drain_timeout_with_standalone_blocker_says_stop_it() {
        // A standalone instance will never be stopped by this command, so the
        // error must name it and must NOT claim a rerun would help.
        use crate::commands::up::plan::{CurrentNetwork, NetworkAction};
        use unisrv_api::models::{
            InstanceInfo, InstanceListEntry, InstanceListResponse, InstanceState,
        };

        let net_id = Uuid::new_v4();
        let inst_id = Uuid::new_v4();
        let mut client = MockApiClient::logged_in();
        for _ in 0..(super::NETWORK_DRAIN_MAX_ATTEMPTS + 1) {
            client = client.push_get_network(Ok(network_response(
                net_id,
                "internal",
                "10.0.0.0/16",
                vec![InstanceInfo {
                    id: inst_id,
                    internal_ip: "10.0.0.1".into(),
                }],
            )));
        }
        let client = client.with_list_instances(Ok(InstanceListResponse {
            instances: vec![InstanceListEntry {
                id: inst_id,
                name: Some("redis-cache".into()),
                state: InstanceState("running".into()),
                container_image: "redis:7".into(),
                created_at: NaiveDateTime::default(),
                deployment: None, // standalone
            }],
        }));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![],
            network_actions: vec![NetworkAction::Delete(CurrentNetwork {
                id: net_id,
                name: "internal".into(),
                ipv4_cidr: "10.0.0.0/16".into(),
            })],
            instance_stops: vec![],
        };

        let err = apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("redis-cache"), "names the instance: {msg}");
        assert!(msg.contains("standalone"), "explains why: {msg}");
        assert!(!msg.contains("Rerun"), "a rerun won't help here: {msg}");
    }

    #[tokio::test]
    async fn network_delete_runs_after_instance_stops() {
        // In destroy, the instances blocking a network delete can be the
        // standalone ones the stop pass tears down — so network deletes must
        // come after the stops, or the drain would wait on instances we were
        // about to stop ourselves.
        use crate::commands::up::plan::{CurrentNetwork, NetworkAction};

        let net_id = Uuid::new_v4();
        let inst_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .push_deprovision_instance(Ok(()))
            .push_get_network(Ok(network_response(
                net_id,
                "internal",
                "10.0.0.0/16",
                vec![],
            )))
            .push_delete_network(Ok(()));

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![],
            network_actions: vec![NetworkAction::Delete(CurrentNetwork {
                id: net_id,
                name: "internal".into(),
                ipv4_cidr: "10.0.0.0/16".into(),
            })],
            instance_stops: vec![InstanceStop {
                id: inst_id,
                name: Some("redis-cache".into()),
            }],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.delete_network_calls[0].1, net_id);
        let order = &calls.call_order;
        let stop = order
            .iter()
            .position(|m| *m == "deprovision_instance")
            .unwrap();
        let drain = order.iter().position(|m| *m == "get_network").unwrap();
        assert!(
            stop < drain,
            "network drain/delete must follow instance stops: {order:?}"
        );
    }

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
            network_actions: vec![],
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Create(DesiredService {
                name: "web".into(),
                hosts: vec!["shop.acme.com".into()],
                region: "dev".into(),
                configuration: http_config(),
            })],
            deployment_actions: vec![],
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            .push_delete_service(Ok(()))
            .push_provision_service(Ok(ServiceProvisionResponse { service_id: new_id }))
            .push_link_host(Ok(host_response(h_id, "shop.acme.com")));

        let mut existing = BTreeMap::new();
        existing.insert("web".to_string(), old_id);

        let plan = Plan {
            network_actions: vec![],
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
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            network_actions: vec![],
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
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            network_actions: vec![],
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
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            network_actions: vec![],
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
            instance_stops: vec![],
        };

        apply(plan, &client, &hosts, &NoSleep, &SilentProgress)
            .await
            .unwrap(); // must NOT error
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
            network_actions: vec![],
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
            deployment_actions: vec![DeploymentAction::Create {
                service: Some(ResolvedServiceBinding {
                    service: ResourceRef::Pending { name: "web".into() },
                    target_group: "default".into(),
                }),
                network: None,
                desired: DesiredDeployment {
                    network: None,
                    name: "web".into(),
                    configuration: dep_config("nginx:1"),
                    service_binding: Some(DesiredServiceBinding {
                        service_name: "web".into(),
                        target_group: "default".into(),
                    }),
                },
            }],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            network_actions: vec![],
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
            instance_stops: vec![InstanceStop {
                id: inst_id,
                name: Some("worker-0".into()),
            }],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            network_actions: vec![],
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
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            network_actions: vec![],
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
                network: None,
                id: dep_id,
                desired: DesiredDeployment {
                    network: None,
                    name: "web".into(),
                    configuration: dep_config("nginx:2"),
                    service_binding: None,
                },
                current: CurrentDeployment {
                    network_binding: None,
                    id: dep_id,
                    name: "web".into(),
                    configuration: dep_config("nginx:1"),
                    service_binding: None,
                },
            }],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            .push_delete_deployment(Ok(()))
            .push_delete_service(Ok(()))
            .push_provision_service(Ok(ServiceProvisionResponse {
                service_id: new_svc_id,
            }))
            .push_create_deployment(Ok(CreateDeploymentResponse { id: new_dep_id }));

        let plan = Plan {
            network_actions: vec![],
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
                network: None,
                // The service is recreated this run → Pending, resolved to
                // the NEW uuid minted by the service recreate phase.
                service: Some(ResolvedServiceBinding {
                    service: ResourceRef::Pending { name: "web".into() },
                    target_group: "default".into(),
                }),
                current: CurrentDeployment {
                    network_binding: None,
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
                    network: None,
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
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            network_actions: vec![],
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
                network_binding: None,
                id: dep_id,
                name: "old".into(),
                configuration: dep_config("img:1"),
                service_binding: None,
            })],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

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
            network_actions: vec![],
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Create {
                service: None,
                network: None,
                desired: DesiredDeployment {
                    network: None,
                    name: "worker".into(),
                    configuration: dep_config("w:1"),
                    service_binding: None,
                },
            }],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();
        let (_env, req) = &calls.create_deployment_calls[0];
        assert!(req.service.is_none());
    }

    /// Drives every variant of `ServiceAction` and `DeploymentAction` through
    /// `apply()` in a single run. Verifies (a) that each action issues the
    /// expected API call and (b) the documented phase ordering:
    /// service-creates → service-updates → deployment-deletes →
    /// deployment-updates → service-recreates → deployment-creates →
    /// service-deletes.
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

        // Two provision_service calls: phase 3 for create-svc, phase 8 for
        // recreate-svc. FIFO order, so push create-svc first.
        let client = MockApiClient::logged_in()
            .push_update_service(Ok(()))
            .push_delete_deployment(Ok(()))
            .push_delete_deployment(Ok(()))
            .push_update_deployment(Ok(()))
            .push_delete_service(Ok(()))
            .push_delete_service(Ok(()))
            .push_provision_service(Ok(ServiceProvisionResponse {
                service_id: new_create_svc_id,
            }))
            .push_provision_service(Ok(ServiceProvisionResponse {
                service_id: new_recreate_svc_id,
            }))
            // Two create_deployment calls in phase 10: create-dep then recreate-dep.
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
            network_actions: vec![],
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
                DeploymentAction::Create {
                    // Binds to the just-created create-svc to exercise the
                    // Pending → minted-id handoff between phases 3 and 10.
                    service: Some(ResolvedServiceBinding {
                        service: ResourceRef::Pending {
                            name: "create-svc".into(),
                        },
                        target_group: "default".into(),
                    }),
                    network: None,
                    desired: DesiredDeployment {
                        network: None,
                        name: "create-dep".into(),
                        configuration: dep_config("nginx:new"),
                        service_binding: Some(DesiredServiceBinding {
                            service_name: "create-svc".into(),
                            target_group: "default".into(),
                        }),
                    },
                },
                DeploymentAction::Update {
                    network: None,
                    id: update_dep_id,
                    desired: DesiredDeployment {
                        network: None,
                        name: "update-dep".into(),
                        configuration: dep_config("nginx:2"),
                        service_binding: Some(DesiredServiceBinding {
                            service_name: "stable-svc".into(),
                            target_group: "default".into(),
                        }),
                    },
                    current: CurrentDeployment {
                        network_binding: None,
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
                    network: None,
                    service: Some(ResolvedServiceBinding {
                        service: ResourceRef::Pending {
                            name: "create-svc".into(),
                        },
                        target_group: "default".into(),
                    }),
                    current: CurrentDeployment {
                        network_binding: None,
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
                        network: None,
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
                    network_binding: None,
                    id: delete_dep_id,
                    name: "delete-dep".into(),
                    configuration: dep_config("delete:1"),
                    service_binding: None,
                }),
            ],
            instance_stops: vec![],
        };

        apply(plan, &client, &[], &NoSleep, &SilentProgress)
            .await
            .unwrap();

        let calls = client.calls.lock().unwrap();

        // ── Each action ran exactly the expected API calls ──
        // env was Use, no create_environment.
        assert_eq!(calls.create_environment_calls.len(), 0);
        // Two provision calls: create-svc (phase 3) and recreate-svc (phase 8).
        let provisioned: Vec<&str> = calls
            .provision_service_calls
            .iter()
            .map(|(_, req)| req.name.as_str())
            .collect();
        assert_eq!(provisioned, vec!["create-svc", "recreate-svc"]);

        assert_eq!(calls.update_service_calls.len(), 1);
        assert_eq!(calls.update_service_calls[0].1, update_svc_id);

        // Phase 6 deletes: explicit delete-dep + recreate-dep's old id.
        let deleted_deps: Vec<Uuid> = calls
            .delete_deployment_calls
            .iter()
            .map(|(_, id)| *id)
            .collect();
        assert!(deleted_deps.contains(&delete_dep_id));
        assert!(deleted_deps.contains(&old_recreate_dep_id));
        assert_eq!(deleted_deps.len(), 2);

        // Phase 8 + phase 11 service deletes: recreate-svc.old, then delete-svc.
        let deleted_svcs: Vec<Uuid> = calls
            .delete_service_calls
            .iter()
            .map(|(_, id)| *id)
            .collect();
        assert_eq!(deleted_svcs, vec![old_recreate_svc_id, delete_svc_id]);

        // Phase 10 deployment creates: create-dep, then recreate-dep.
        // recreate-dep must bind to the *new* create-svc id, since it was
        // produced by phase 3.
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

        // 3 → 4: every provision_service before any update_service?
        // No — phase 3 has only one provision (create-svc), then phase 4
        // updates, then phase 8 provisions again. So: first provision_service
        // < update_service < last provision_service. That's the boundary.
        assert!(first("provision_service") < first("update_service"));
        assert!(first("update_service") < first("delete_deployment"));
        // 6 → 7: every deployment delete before any deployment update (the
        // updates may free networks; both precede the network phases).
        assert!(last("delete_deployment") < first("update_deployment"));
        // 7 → 8: deployment updates land before service recreates begin.
        assert!(first("update_deployment") < first("delete_service"));
        // 8 internal: recreate-svc deleted before being re-provisioned.
        assert!(first("delete_service") < last("provision_service"));
        // 8 → 10: recreate-svc provisioned before any deployment is created.
        assert!(last("provision_service") < first("create_deployment"));
        // 10 → 11: deployment creates before the final service delete (delete-svc).
        assert!(last("create_deployment") < last("delete_service"));
    }
}
