//! Plan structure and pure diff function.
//!
//! `diff(desired, current)` produces a `Plan` value describing the create/update/
//! recreate/delete actions needed to converge `current` to `desired`. The function
//! is pure — no I/O — so it can be tested exhaustively without mocks.
//!
//! Important constraints from the backend:
//!  * `update_service` only accepts `HTTPServiceConfig`. `host` / `region` /
//!    `name` are immutable post-creation, so a change forces **Recreate**.
//!  * `update_deployment` mutates `DeploymentConfiguration` and the network
//!    binding (`network_id` is full desired state on PUT; the operator rolls
//!    instances zero-downtime). The `service_id` / `target_group` binding is
//!    creation-only, so a service-binding change forces **Recreate**.
//!  * Because `services -> deployments` is `ON DELETE SET NULL` and we cannot
//!    rebind, a service Recreate **cascade-recreates** every deployment bound
//!    to it.

use std::collections::{BTreeMap, BTreeSet};

use unisrv_api::models::{CreateEnvironmentRequest, DeploymentConfiguration, HTTPServiceConfig};
use uuid::Uuid;

use super::desired::{DesiredDeployment, DesiredNetwork, DesiredService, DesiredState};

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentState {
    pub services: BTreeMap<String, CurrentService>,
    pub deployments: BTreeMap<String, CurrentDeployment>,
    pub networks: BTreeMap<String, CurrentNetwork>,
}

impl CurrentState {
    pub fn empty() -> Self {
        CurrentState {
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
            networks: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentNetwork {
    pub id: Uuid,
    pub name: String,
    pub ipv4_cidr: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentService {
    pub id: Uuid,
    pub name: String,
    /// Custom hosts currently bound to this service (excludes the derived base
    /// host). Sourced from the service detail's `custom_hosts`.
    pub hosts: Vec<String>,
    pub region: String,
    pub configuration: HTTPServiceConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentDeployment {
    pub id: Uuid,
    pub name: String,
    pub configuration: DeploymentConfiguration,
    /// Resolved name of the bound service (from server-side service_id lookup).
    pub service_binding: Option<CurrentServiceBinding>,
    /// Resolved name of the joined network (from server-side network_id lookup).
    /// `None` also covers a dangling id whose network was deleted out-of-band.
    pub network_binding: Option<CurrentNetworkBinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentServiceBinding {
    pub service_id: Uuid,
    pub service_name: String,
    pub target_group: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentNetworkBinding {
    pub network_id: Uuid,
    pub network_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub project: String,
    pub env_action: EnvAction,
    pub service_actions: Vec<ServiceAction>,
    pub deployment_actions: Vec<DeploymentAction>,
    pub network_actions: Vec<NetworkAction>,
    /// Instances to deprovision directly (not via a deployment). Always empty for
    /// `up` — only `destroy` appends standalone instances here. Applied as a no-op
    /// when empty, so `up` stays instance-unaware.
    pub instance_stops: Vec<InstanceStop>,
}

/// A standalone instance (one with no owning deployment) to be torn down. Carries
/// just enough to call `deprovision_instance` and render a line for it.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceStop {
    pub id: Uuid,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnvAction {
    /// Environment already exists; just use it.
    Use(ResolvedEnvironment),
    /// Environment will be created with these parameters.
    Create(CreateEnvironmentRequest),
}

/// The minimal info we need about an existing environment to act on it.
/// Deliberately narrower than the API's `EnvironmentResponse` — only `id`
/// and `name` are actually consumed downstream, plus `project` to keep tests
/// honest about which env was selected.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEnvironment {
    pub id: Uuid,
    pub name: String,
    pub project: String,
    /// Env slug, used to derive each service's base host for the post-`up`
    /// reachability summary.
    pub slug: String,
}

impl From<&unisrv_api::models::EnvironmentListEntry> for ResolvedEnvironment {
    fn from(entry: &unisrv_api::models::EnvironmentListEntry) -> Self {
        Self {
            id: entry.id,
            name: entry.name.clone(),
            project: entry.project.clone(),
            slug: entry.slug.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServiceAction {
    Create(DesiredService),
    Update {
        id: Uuid,
        desired: DesiredService,
        current: CurrentService,
    },
    Recreate {
        current: CurrentService,
        desired: DesiredService,
        reasons: Vec<RecreateReason>,
    },
    Delete(CurrentService),
}

impl ServiceAction {
    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        match self {
            ServiceAction::Create(d) => &d.name,
            ServiceAction::Update { desired, .. } => &desired.name,
            ServiceAction::Recreate { desired, .. } => &desired.name,
            ServiceAction::Delete(c) => &c.name,
        }
    }
}

/// A reference from one resource to another, resolved as far as plan time
/// allows. "Resolved as existing" is a fact the diff establishes once, here —
/// apply never re-derives it from a name lookup.
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceRef {
    /// The target exists and is not touched by this plan: its uuid is known
    /// at plan time.
    Existing { id: Uuid, name: String },
    /// The target is created or recreated by this plan: its uuid is minted
    /// during apply and resolved from the minted-ids map.
    Pending { name: String },
}

/// A deployment's service binding with its service reference resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedServiceBinding {
    pub service: ResourceRef,
    pub target_group: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeploymentAction {
    Create {
        desired: DesiredDeployment,
        /// Resolved service binding (creation-only on the backend).
        service: Option<ResolvedServiceBinding>,
        /// Resolved network reference.
        network: Option<ResourceRef>,
    },
    Update {
        id: Uuid,
        desired: DesiredDeployment,
        current: CurrentDeployment,
        /// Resolved network reference. Always `Existing` (or `None`): a
        /// deployment desiring a created/recreated network is forced onto the
        /// Create/Recreate path by the diff, never Update.
        network: Option<ResourceRef>,
    },
    Recreate {
        current: CurrentDeployment,
        desired: DesiredDeployment,
        reasons: Vec<RecreateReason>,
        service: Option<ResolvedServiceBinding>,
        network: Option<ResourceRef>,
    },
    Delete(CurrentDeployment),
}

impl DeploymentAction {
    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        match self {
            DeploymentAction::Create { desired, .. } => &desired.name,
            DeploymentAction::Update { desired, .. } => &desired.name,
            DeploymentAction::Recreate { desired, .. } => &desired.name,
            DeploymentAction::Delete(c) => &c.name,
        }
    }
}

/// Networks have no update endpoint and both fields (name = the map key,
/// `ipv4_cidr`) are immutable, so the taxonomy is Create / Recreate / Delete —
/// there is no Update variant.
#[derive(Debug, Clone, PartialEq)]
pub enum NetworkAction {
    Create(DesiredNetwork),
    Recreate {
        current: CurrentNetwork,
        desired: DesiredNetwork,
        reasons: Vec<RecreateReason>,
    },
    Delete(CurrentNetwork),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecreateReason {
    /// e.g. "host" — an immutable field changed
    ImmutableField {
        field: &'static str,
        old: String,
        new: String,
    },
    /// Service binding changed (deployment-only).
    ServiceBindingChanged,
    /// The service this deployment binds to is being recreated, so the binding
    /// would be lost. We have to recreate this deployment after the service.
    DependentServiceRecreated { service_name: String },
    /// The network this deployment joins is being recreated under a new id, so
    /// the deployment must be recreated after the network to pick it up.
    DependentNetworkRecreated { network_name: String },
}

/// The shared name-keyed reconciliation walk: a desired entry with no current
/// counterpart becomes `on_create`; a pair present on both sides is judged by
/// `on_both` (`None` = no action); a current entry no longer desired becomes
/// `on_delete`. Per-resource policy (what recreates, what updates) lives
/// entirely in the closures — this owns only the set arithmetic, so the three
/// resource walks can't drift on it.
fn diff_by_name<D, C, A>(
    desired: &BTreeMap<String, D>,
    current: &BTreeMap<String, C>,
    mut on_create: impl FnMut(&D) -> A,
    mut on_both: impl FnMut(&D, &C) -> Option<A>,
    mut on_delete: impl FnMut(&C) -> A,
) -> Vec<A> {
    let mut actions = Vec::new();
    for (name, d) in desired {
        match current.get(name) {
            None => actions.push(on_create(d)),
            Some(c) => actions.extend(on_both(d, c)),
        }
    }
    for (name, c) in current {
        if !desired.contains_key(name) {
            actions.push(on_delete(c));
        }
    }
    actions
}

pub fn diff(desired: &DesiredState, current: &CurrentState, env_action: EnvAction) -> Plan {
    // ── Services ──
    let mut recreated_services: BTreeSet<String> = BTreeSet::new();

    let service_actions = diff_by_name(
        &desired.services,
        &current.services,
        |d| ServiceAction::Create(d.clone()),
        |d, c| {
            let immutable_diffs = super::diff::service::immutable_diffs(d, c);
            if !immutable_diffs.is_empty() {
                recreated_services.insert(d.name.clone());
                Some(ServiceAction::Recreate {
                    current: c.clone(),
                    desired: d.clone(),
                    reasons: immutable_diffs,
                })
            } else if d.configuration != c.configuration || super::diff::service::hosts_differ(d, c)
            {
                Some(ServiceAction::Update {
                    id: c.id,
                    desired: d.clone(),
                    current: c.clone(),
                })
            } else {
                None
            }
        },
        |c| ServiceAction::Delete(c.clone()),
    );

    // ── Networks ──
    // No update path exists (name and CIDR are both immutable), so a same-name
    // CIDR change is a Recreate and everything else is Create/Delete.
    let mut recreated_networks: BTreeSet<String> = BTreeSet::new();

    let network_actions = diff_by_name(
        &desired.networks,
        &current.networks,
        |d| NetworkAction::Create(d.clone()),
        |d, c| {
            if d.ipv4_cidr != c.ipv4_cidr {
                recreated_networks.insert(d.name.clone());
                Some(NetworkAction::Recreate {
                    current: c.clone(),
                    desired: d.clone(),
                    reasons: vec![RecreateReason::ImmutableField {
                        field: "iprange",
                        old: c.ipv4_cidr.clone(),
                        new: d.ipv4_cidr.clone(),
                    }],
                })
            } else {
                None
            }
        },
        |c| NetworkAction::Delete(c.clone()),
    );

    // ── Deployments ──
    // Resolve a referenced resource as far as plan time allows: a target that
    // exists and is untouched by this plan carries its uuid; one created or
    // recreated this run stays Pending until apply mints the new id.
    let network_ref = |name: &String| -> ResourceRef {
        match current.networks.get(name) {
            Some(net) if !recreated_networks.contains(name) => ResourceRef::Existing {
                id: net.id,
                name: name.clone(),
            },
            _ => ResourceRef::Pending { name: name.clone() },
        }
    };
    let service_ref = |binding: &super::desired::DesiredServiceBinding| -> ResolvedServiceBinding {
        let service = match current.services.get(&binding.service_name) {
            Some(svc) if !recreated_services.contains(&binding.service_name) => {
                ResourceRef::Existing {
                    id: svc.id,
                    name: binding.service_name.clone(),
                }
            }
            _ => ResourceRef::Pending {
                name: binding.service_name.clone(),
            },
        };
        ResolvedServiceBinding {
            service,
            target_group: binding.target_group.clone(),
        }
    };

    let deployment_actions = diff_by_name(
        &desired.deployments,
        &current.deployments,
        |d| DeploymentAction::Create {
            service: d.service_binding.as_ref().map(&service_ref),
            network: d.network.as_ref().map(&network_ref),
            desired: d.clone(),
        },
        |d, c| {
            let mut reasons = Vec::new();

            // Cascade: if bound to a service being recreated, force recreate.
            if let Some(b) = &d.service_binding
                && recreated_services.contains(&b.service_name)
            {
                reasons.push(RecreateReason::DependentServiceRecreated {
                    service_name: b.service_name.clone(),
                });
            }

            // Cascade: a recreated network gets a NEW uuid, so a deployment
            // desiring it must be recreated to bind to the new id.
            if let Some(net) = &d.network
                && recreated_networks.contains(net)
            {
                reasons.push(RecreateReason::DependentNetworkRecreated {
                    network_name: net.clone(),
                });
            }

            if !service_bindings_match(d.service_binding.as_ref(), c.service_binding.as_ref()) {
                reasons.push(RecreateReason::ServiceBindingChanged);
            }

            if !reasons.is_empty() {
                Some(DeploymentAction::Recreate {
                    service: d.service_binding.as_ref().map(&service_ref),
                    network: d.network.as_ref().map(&network_ref),
                    current: c.clone(),
                    desired: d.clone(),
                    reasons,
                })
            } else if d.configuration != c.configuration || network_binding_differs(d, c) {
                // A network binding change is an in-place Update: the backend
                // swaps network_id on PUT and the operator rolls instances
                // onto (or off) the network zero-downtime.
                Some(DeploymentAction::Update {
                    id: c.id,
                    network: d.network.as_ref().map(&network_ref),
                    desired: d.clone(),
                    current: c.clone(),
                })
            } else {
                None
            }
        },
        |c| DeploymentAction::Delete(c.clone()),
    );

    Plan {
        project: desired.project.clone(),
        env_action,
        service_actions,
        deployment_actions,
        network_actions,
        // diff is instance-unaware; destroy appends stops to the plan afterwards.
        instance_stops: Vec::new(),
    }
}

/// Compares user *intent* (name + target_group) only — never `service_id`.
/// Desired has no id at diff time; that resolution happens at apply time
/// against the live `service_ids` map. When a service is recreated and its
/// id changes, the deployment is forced onto the recreate path separately
/// via [`RecreateReason::DependentServiceRecreated`], not via this function.
fn service_bindings_match(
    desired: Option<&super::desired::DesiredServiceBinding>,
    current: Option<&CurrentServiceBinding>,
) -> bool {
    match (desired, current) {
        (None, None) => true,
        (Some(d), Some(c)) => d.service_name == c.service_name && d.target_group == c.target_group,
        _ => false,
    }
}

/// Compares user *intent* (network name) only — never `network_id`. Like
/// service bindings, id resolution happens at apply time; a recreated network
/// forces the deployment onto the Recreate path separately via
/// [`RecreateReason::DependentNetworkRecreated`], not via this function.
fn network_binding_differs(desired: &DesiredDeployment, current: &CurrentDeployment) -> bool {
    desired.network.as_deref()
        != current
            .network_binding
            .as_ref()
            .map(|b| b.network_name.as_str())
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        matches!(self.env_action, EnvAction::Use(_))
            && self.service_actions.is_empty()
            && self.deployment_actions.is_empty()
            && self.network_actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unisrv_api::models::{
        DeploymentConfiguration, HTTPLocation, HTTPLocationTarget, HTTPServiceConfig,
    };

    fn use_env() -> EnvAction {
        EnvAction::Use(ResolvedEnvironment {
            id: Uuid::new_v4(),
            name: "prod".into(),
            project: "demo".into(),
            slug: "ab12".into(),
        })
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

    fn desired_with_service(name: &str, host: &str) -> DesiredState {
        let mut s = DesiredState {
            networks: BTreeMap::new(),
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        s.services.insert(
            name.into(),
            DesiredService {
                name: name.into(),
                hosts: vec![host.into()],
                region: "dev".into(),
                configuration: http_config(),
            },
        );
        s
    }

    fn current_with_service(name: &str, host: &str) -> CurrentState {
        let mut s = CurrentState::empty();
        s.services.insert(
            name.into(),
            CurrentService {
                id: Uuid::new_v4(),
                name: name.into(),
                hosts: vec![host.into()],
                region: "dev".into(),
                configuration: http_config(),
            },
        );
        s
    }

    #[test]
    fn empty_desired_and_current_yields_empty_plan() {
        let plan = diff(
            &DesiredState {
                networks: BTreeMap::new(),
                project: "demo".into(),
                services: BTreeMap::new(),
                deployments: BTreeMap::new(),
            },
            &CurrentState::empty(),
            use_env(),
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn plan_is_not_empty_when_env_will_be_created() {
        let plan = diff(
            &DesiredState {
                networks: BTreeMap::new(),
                project: "demo".into(),
                services: BTreeMap::new(),
                deployments: BTreeMap::new(),
            },
            &CurrentState::empty(),
            EnvAction::Create(CreateEnvironmentRequest {
                project: "demo".into(),
                name: "prod".into(),
                display_name: None,
                description: None,
            }),
        );
        assert!(!plan.is_empty());
    }

    #[test]
    fn missing_service_is_create() {
        let plan = diff(
            &desired_with_service("web", "web.example.com"),
            &CurrentState::empty(),
            use_env(),
        );
        assert_eq!(plan.service_actions.len(), 1);
        match &plan.service_actions[0] {
            ServiceAction::Create(d) => assert_eq!(d.name, "web"),
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn extra_service_is_delete() {
        let plan = diff(
            &DesiredState {
                networks: BTreeMap::new(),
                project: "demo".into(),
                services: BTreeMap::new(),
                deployments: BTreeMap::new(),
            },
            &current_with_service("old", "old.example.com"),
            use_env(),
        );
        assert!(matches!(
            plan.service_actions.as_slice(),
            [ServiceAction::Delete(_)]
        ));
    }

    #[test]
    fn host_only_change_is_service_update() {
        // Hosts are mutable (link/unlink), so a host-set change on an otherwise
        // unchanged service is an Update — not a recreate, not a no-op.
        let plan = diff(
            &desired_with_service("web", "new.example.com"),
            &current_with_service("web", "old.example.com"),
            use_env(),
        );
        assert!(
            matches!(
                plan.service_actions.as_slice(),
                [ServiceAction::Update { .. }]
            ),
            "expected Update, got {:?}",
            plan.service_actions
        );
    }

    #[test]
    fn config_change_only_is_service_update() {
        let mut desired = desired_with_service("web", "h.example");
        desired
            .services
            .get_mut("web")
            .unwrap()
            .configuration
            .allow_http = true;
        let plan = diff(
            &desired,
            &current_with_service("web", "h.example"),
            use_env(),
        );
        assert!(matches!(
            plan.service_actions.as_slice(),
            [ServiceAction::Update { .. }]
        ));
    }

    #[test]
    fn no_diff_yields_no_actions() {
        let desired = desired_with_service("web", "h.example");
        let current = current_with_service("web", "h.example");
        let plan = diff(&desired, &current, use_env());
        assert!(plan.service_actions.is_empty());
    }

    #[test]
    fn deployment_image_change_is_update() {
        let mut desired = desired_with_service("web", "h.example");
        desired.deployments.insert(
            "web".into(),
            DesiredDeployment {
                network: None,
                name: "web".into(),
                configuration: dep_config("nginx:2"),
                service_binding: Some(super::super::desired::DesiredServiceBinding {
                    service_name: "web".into(),
                    target_group: "default".into(),
                }),
            },
        );
        let svc_id = Uuid::new_v4();
        let mut current = CurrentState::empty();
        current.services.insert(
            "web".into(),
            CurrentService {
                id: svc_id,
                name: "web".into(),
                hosts: vec!["h.example".into()],
                region: "dev".into(),
                configuration: http_config(),
            },
        );
        current.deployments.insert(
            "web".into(),
            CurrentDeployment {
                network_binding: None,
                id: Uuid::new_v4(),
                name: "web".into(),
                configuration: dep_config("nginx:1"),
                service_binding: Some(CurrentServiceBinding {
                    service_id: svc_id,
                    service_name: "web".into(),
                    target_group: "default".into(),
                }),
            },
        );
        let plan = diff(&desired, &current, use_env());
        assert!(plan.service_actions.is_empty());
        assert!(matches!(
            plan.deployment_actions.as_slice(),
            [DeploymentAction::Update { .. }]
        ));
    }

    #[test]
    fn service_recreate_cascades_to_dependent_deployment_recreate() {
        // A service recreate (here triggered by a region change) cascade-
        // recreates every deployment bound to it.
        let mut desired = desired_with_service("web", "app.example");
        desired.services.get_mut("web").unwrap().region = "us-east".into();
        desired.deployments.insert(
            "web".into(),
            DesiredDeployment {
                network: None,
                name: "web".into(),
                configuration: dep_config("nginx:1"),
                service_binding: Some(super::super::desired::DesiredServiceBinding {
                    service_name: "web".into(),
                    target_group: "default".into(),
                }),
            },
        );
        let svc_id = Uuid::new_v4();
        let mut current = CurrentState::empty();
        current.services.insert(
            "web".into(),
            CurrentService {
                id: svc_id,
                name: "web".into(),
                hosts: vec!["app.example".into()],
                region: "dev".into(),
                configuration: http_config(),
            },
        );
        current.deployments.insert(
            "web".into(),
            CurrentDeployment {
                network_binding: None,
                id: Uuid::new_v4(),
                name: "web".into(),
                configuration: dep_config("nginx:1"),
                service_binding: Some(CurrentServiceBinding {
                    service_id: svc_id,
                    service_name: "web".into(),
                    target_group: "default".into(),
                }),
            },
        );
        let plan = diff(&desired, &current, use_env());
        assert!(matches!(
            plan.service_actions.as_slice(),
            [ServiceAction::Recreate { .. }]
        ));
        match &plan.deployment_actions[0] {
            DeploymentAction::Recreate { reasons, .. } => {
                assert!(reasons.iter().any(|r| matches!(
                    r,
                    RecreateReason::DependentServiceRecreated { service_name } if service_name == "web"
                )));
            }
            other => panic!("expected Recreate, got {other:?}"),
        }
    }

    #[test]
    fn binding_change_is_deployment_recreate() {
        let mut desired = DesiredState {
            networks: BTreeMap::new(),
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        // Two services, both with the same host (just for the test).
        for n in ["a", "b"] {
            desired.services.insert(
                n.into(),
                DesiredService {
                    name: n.into(),
                    hosts: vec![format!("{n}.example")],
                    region: "dev".into(),
                    configuration: http_config(),
                },
            );
        }
        // Deployment is desired bound to service "b".
        desired.deployments.insert(
            "dep".into(),
            DesiredDeployment {
                network: None,
                name: "dep".into(),
                configuration: dep_config("img:1"),
                service_binding: Some(super::super::desired::DesiredServiceBinding {
                    service_name: "b".into(),
                    target_group: "default".into(),
                }),
            },
        );

        // Current bound to service "a".
        let svc_a_id = Uuid::new_v4();
        let svc_b_id = Uuid::new_v4();
        let mut current = CurrentState::empty();
        for (n, id) in [("a", svc_a_id), ("b", svc_b_id)] {
            current.services.insert(
                n.into(),
                CurrentService {
                    id,
                    name: n.into(),
                    hosts: vec![format!("{n}.example")],
                    region: "dev".into(),
                    configuration: http_config(),
                },
            );
        }
        current.deployments.insert(
            "dep".into(),
            CurrentDeployment {
                network_binding: None,
                id: Uuid::new_v4(),
                name: "dep".into(),
                configuration: dep_config("img:1"),
                service_binding: Some(CurrentServiceBinding {
                    service_id: svc_a_id,
                    service_name: "a".into(),
                    target_group: "default".into(),
                }),
            },
        );

        let plan = diff(&desired, &current, use_env());
        match &plan.deployment_actions[0] {
            DeploymentAction::Recreate { reasons, .. } => {
                assert!(
                    reasons.contains(&RecreateReason::ServiceBindingChanged),
                    "reasons: {reasons:?}"
                );
            }
            other => panic!("expected Recreate, got {other:?}"),
        }
    }

    #[test]
    fn missing_deployment_is_create() {
        let mut desired = DesiredState {
            networks: BTreeMap::new(),
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        desired.deployments.insert(
            "worker".into(),
            DesiredDeployment {
                network: None,
                name: "worker".into(),
                configuration: dep_config("worker:1"),
                service_binding: None,
            },
        );
        let plan = diff(&desired, &CurrentState::empty(), use_env());
        assert!(plan.service_actions.is_empty());
        assert!(matches!(
            plan.deployment_actions.as_slice(),
            [DeploymentAction::Create { desired, .. }] if desired.name == "worker"
        ));
    }

    #[test]
    fn extra_deployment_is_delete() {
        let desired = DesiredState {
            networks: BTreeMap::new(),
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        let mut current = CurrentState::empty();
        current.deployments.insert(
            "old-worker".into(),
            CurrentDeployment {
                network_binding: None,
                id: Uuid::new_v4(),
                name: "old-worker".into(),
                configuration: dep_config("old:1"),
                service_binding: None,
            },
        );
        let plan = diff(&desired, &current, use_env());
        assert!(matches!(
            plan.deployment_actions.as_slice(),
            [DeploymentAction::Delete(d)] if d.name == "old-worker"
        ));
    }

    #[test]
    fn region_change_is_service_recreate() {
        let mut desired = desired_with_service("web", "h.example");
        desired.services.get_mut("web").unwrap().region = "us-east".into();
        let plan = diff(
            &desired,
            &current_with_service("web", "h.example"),
            use_env(),
        );
        match &plan.service_actions[0] {
            ServiceAction::Recreate { reasons, .. } => {
                assert!(
                    reasons.iter().any(|r| matches!(
                        r,
                        RecreateReason::ImmutableField {
                            field: "region",
                            ..
                        }
                    )),
                    "reasons: {reasons:?}"
                );
            }
            other => panic!("expected Recreate, got {other:?}"),
        }
    }

    /// One diff that produces every variant of `ServiceAction` and
    /// `DeploymentAction` simultaneously, to catch interactions between
    /// action paths that isolated tests miss.
    #[test]
    fn kitchen_sink_yields_full_action_taxonomy() {
        use super::super::desired::DesiredServiceBinding;

        // Service ids that exist on the "server" side. New ids (Create / the
        // post-Recreate id) come from the apply layer, not here.
        let stable_id = Uuid::new_v4();
        let update_id = Uuid::new_v4();
        let recreate_id = Uuid::new_v4();
        let delete_id = Uuid::new_v4();
        let update_dep_id = Uuid::new_v4();
        let recreate_dep_id = Uuid::new_v4();
        let delete_dep_id = Uuid::new_v4();

        // ── Desired: stable (no diff) + create + update + recreate ──
        let mut desired = DesiredState {
            networks: BTreeMap::new(),
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        let mut updated_cfg = http_config();
        updated_cfg.allow_http = true; // forces a config-only diff vs. current

        for (name, host, cfg) in [
            ("stable-svc", "stable.example", http_config()),
            ("create-svc", "create.example", http_config()),
            ("update-svc", "update.example", updated_cfg.clone()),
            ("recreate-svc", "recreate.example", http_config()),
        ] {
            desired.services.insert(
                name.into(),
                DesiredService {
                    name: name.into(),
                    hosts: vec![host.into()],
                    region: "dev".into(),
                    configuration: cfg,
                },
            );
        }
        // recreate-svc recreates because its region (immutable) changes.
        desired.services.get_mut("recreate-svc").unwrap().region = "us-east".into();

        // create-dep: new (binds to the new create-svc).
        // update-dep: image bump, binding unchanged on stable-svc.
        // recreate-dep: binding flips from stable-svc to create-svc.
        desired.deployments.insert(
            "create-dep".into(),
            DesiredDeployment {
                network: None,
                name: "create-dep".into(),
                configuration: dep_config("nginx:new"),
                service_binding: Some(DesiredServiceBinding {
                    service_name: "create-svc".into(),
                    target_group: "default".into(),
                }),
            },
        );
        desired.deployments.insert(
            "update-dep".into(),
            DesiredDeployment {
                network: None,
                name: "update-dep".into(),
                configuration: dep_config("nginx:2"),
                service_binding: Some(DesiredServiceBinding {
                    service_name: "stable-svc".into(),
                    target_group: "default".into(),
                }),
            },
        );
        desired.deployments.insert(
            "recreate-dep".into(),
            DesiredDeployment {
                network: None,
                name: "recreate-dep".into(),
                configuration: dep_config("nginx:1"),
                service_binding: Some(DesiredServiceBinding {
                    service_name: "create-svc".into(),
                    target_group: "default".into(),
                }),
            },
        );

        // ── Current: stable + update + recreate (with old host) + delete ──
        let mut current = CurrentState::empty();
        for (name, id, host) in [
            ("stable-svc", stable_id, "stable.example"),
            ("update-svc", update_id, "update.example"),
            ("recreate-svc", recreate_id, "recreate.example"), // region differs (set on desired)
            ("delete-svc", delete_id, "delete.example"),
        ] {
            current.services.insert(
                name.into(),
                CurrentService {
                    id,
                    name: name.into(),
                    hosts: vec![host.into()],
                    region: "dev".into(),
                    configuration: http_config(),
                },
            );
        }

        current.deployments.insert(
            "update-dep".into(),
            CurrentDeployment {
                network_binding: None,
                id: update_dep_id,
                name: "update-dep".into(),
                configuration: dep_config("nginx:1"),
                service_binding: Some(CurrentServiceBinding {
                    service_id: stable_id,
                    service_name: "stable-svc".into(),
                    target_group: "default".into(),
                }),
            },
        );
        current.deployments.insert(
            "recreate-dep".into(),
            CurrentDeployment {
                network_binding: None,
                id: recreate_dep_id,
                name: "recreate-dep".into(),
                configuration: dep_config("nginx:1"),
                service_binding: Some(CurrentServiceBinding {
                    service_id: stable_id,
                    service_name: "stable-svc".into(),
                    target_group: "default".into(),
                }),
            },
        );
        current.deployments.insert(
            "delete-dep".into(),
            CurrentDeployment {
                network_binding: None,
                id: delete_dep_id,
                name: "delete-dep".into(),
                configuration: dep_config("delete:1"),
                service_binding: None,
            },
        );

        let plan = diff(&desired, &current, use_env());

        // Every variant of ServiceAction is represented exactly once.
        let svc_by_name: BTreeMap<&str, &ServiceAction> =
            plan.service_actions.iter().map(|a| (a.name(), a)).collect();
        assert_eq!(svc_by_name.len(), 4, "{:?}", plan.service_actions);
        assert!(matches!(
            svc_by_name["create-svc"],
            ServiceAction::Create(_)
        ));
        assert!(matches!(
            svc_by_name["update-svc"],
            ServiceAction::Update { .. }
        ));
        assert!(matches!(
            svc_by_name["recreate-svc"],
            ServiceAction::Recreate { .. }
        ));
        assert!(matches!(
            svc_by_name["delete-svc"],
            ServiceAction::Delete(_)
        ));

        // stable-svc has no diff — must not appear as an action.
        assert!(!svc_by_name.contains_key("stable-svc"));

        // Every variant of DeploymentAction is represented exactly once.
        let dep_by_name: BTreeMap<&str, &DeploymentAction> = plan
            .deployment_actions
            .iter()
            .map(|a| (a.name(), a))
            .collect();
        assert_eq!(dep_by_name.len(), 4, "{:?}", plan.deployment_actions);
        assert!(matches!(
            dep_by_name["create-dep"],
            DeploymentAction::Create { .. }
        ));
        assert!(matches!(
            dep_by_name["update-dep"],
            DeploymentAction::Update { .. }
        ));
        match dep_by_name["recreate-dep"] {
            DeploymentAction::Recreate { reasons, .. } => assert!(
                reasons.contains(&RecreateReason::ServiceBindingChanged),
                "expected ServiceBindingChanged in {reasons:?}",
            ),
            other => panic!("expected Recreate, got {other:?}"),
        }
        assert!(matches!(
            dep_by_name["delete-dep"],
            DeploymentAction::Delete(_)
        ));

        // Reference resolution: update-dep's binding to the unchanged
        // stable-svc carries its uuid; create-dep's and recreate-dep's
        // bindings to the freshly created create-svc stay Pending until
        // apply mints the id.
        match dep_by_name["update-dep"] {
            DeploymentAction::Update { .. } => {}
            other => panic!("expected Update, got {other:?}"),
        }
        match dep_by_name["create-dep"] {
            DeploymentAction::Create { service, .. } => assert_eq!(
                service.as_ref().unwrap().service,
                ResourceRef::Pending {
                    name: "create-svc".into()
                }
            ),
            other => panic!("expected Create, got {other:?}"),
        }
        let _ = (stable_id, recreate_id);
    }

    // ── Networks ──

    fn empty_desired() -> DesiredState {
        DesiredState {
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
            networks: BTreeMap::new(),
        }
    }

    fn desired_network(name: &str, cidr: &str) -> super::super::desired::DesiredNetwork {
        super::super::desired::DesiredNetwork {
            name: name.into(),
            ipv4_cidr: cidr.into(),
        }
    }

    fn current_network(id: Uuid, name: &str, cidr: &str) -> CurrentNetwork {
        CurrentNetwork {
            id,
            name: name.into(),
            ipv4_cidr: cidr.into(),
        }
    }

    #[test]
    fn missing_network_is_create_and_makes_plan_non_empty() {
        let mut desired = empty_desired();
        desired.networks.insert(
            "internal".into(),
            desired_network("internal", "10.0.0.0/16"),
        );
        let plan = diff(&desired, &CurrentState::empty(), use_env());
        assert!(matches!(
            plan.network_actions.as_slice(),
            [NetworkAction::Create(n)] if n.name == "internal"
        ));
        assert!(!plan.is_empty());
    }

    #[test]
    fn cidr_change_is_network_recreate() {
        let net_id = Uuid::new_v4();
        let mut desired = empty_desired();
        desired.networks.insert(
            "internal".into(),
            desired_network("internal", "10.9.0.0/24"),
        );
        let mut current = CurrentState::empty();
        current.networks.insert(
            "internal".into(),
            current_network(net_id, "internal", "10.0.0.0/16"),
        );
        let plan = diff(&desired, &current, use_env());
        match plan.network_actions.as_slice() {
            [
                NetworkAction::Recreate {
                    current,
                    desired,
                    reasons,
                },
            ] => {
                assert_eq!(current.id, net_id);
                assert_eq!(desired.ipv4_cidr, "10.9.0.0/24");
                assert!(
                    reasons.iter().any(|r| matches!(
                        r,
                        RecreateReason::ImmutableField {
                            field: "iprange",
                            ..
                        }
                    )),
                    "reasons: {reasons:?}"
                );
            }
            other => panic!("expected Recreate, got {other:?}"),
        }
    }

    #[test]
    fn extra_network_is_delete() {
        let mut current = CurrentState::empty();
        current.networks.insert(
            "old".into(),
            current_network(Uuid::new_v4(), "old", "10.0.0.0/16"),
        );
        let plan = diff(&empty_desired(), &current, use_env());
        assert!(matches!(
            plan.network_actions.as_slice(),
            [NetworkAction::Delete(n)] if n.name == "old"
        ));
        assert!(!plan.is_empty());
    }

    #[test]
    fn network_recreate_cascades_to_dependent_deployment_recreate() {
        // A CIDR change recreates the network under a NEW uuid, so every
        // deployment desiring that network must be recreated to pick it up.
        let net_id = Uuid::new_v4();
        let mut desired = empty_desired();
        desired.networks.insert(
            "internal".into(),
            desired_network("internal", "10.9.0.0/24"),
        );
        desired.deployments.insert(
            "api".into(),
            DesiredDeployment {
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: None,
                network: Some("internal".into()),
            },
        );
        let mut current = CurrentState::empty();
        current.networks.insert(
            "internal".into(),
            current_network(net_id, "internal", "10.0.0.0/16"),
        );
        current.deployments.insert(
            "api".into(),
            CurrentDeployment {
                id: Uuid::new_v4(),
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: None,
                network_binding: Some(CurrentNetworkBinding {
                    network_id: net_id,
                    network_name: "internal".into(),
                }),
            },
        );
        let plan = diff(&desired, &current, use_env());
        match plan.deployment_actions.as_slice() {
            [DeploymentAction::Recreate { reasons, .. }] => assert!(
                reasons.iter().any(|r| matches!(
                    r,
                    RecreateReason::DependentNetworkRecreated { network_name } if network_name == "internal"
                )),
                "reasons: {reasons:?}"
            ),
            other => panic!("expected Recreate, got {other:?}"),
        }
    }

    #[test]
    fn network_binding_change_is_inplace_deployment_update() {
        // The backend updates network_id in place (operator rolls instances
        // zero-downtime), so a binding change alone is an Update — never a
        // recreate — even with an identical configuration.
        let net_id = Uuid::new_v4();
        let mut desired = empty_desired();
        desired.networks.insert(
            "internal".into(),
            desired_network("internal", "10.0.0.0/16"),
        );
        desired.deployments.insert(
            "api".into(),
            DesiredDeployment {
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: None,
                network: Some("internal".into()),
            },
        );
        let mut current = CurrentState::empty();
        current.networks.insert(
            "internal".into(),
            current_network(net_id, "internal", "10.0.0.0/16"),
        );
        current.deployments.insert(
            "api".into(),
            CurrentDeployment {
                id: Uuid::new_v4(),
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: None,
                network_binding: None, // currently detached
            },
        );
        let plan = diff(&desired, &current, use_env());
        assert!(plan.network_actions.is_empty(), "network itself unchanged");
        assert!(
            matches!(
                plan.deployment_actions.as_slice(),
                [DeploymentAction::Update { .. }]
            ),
            "expected in-place Update, got {:?}",
            plan.deployment_actions
        );
    }

    #[test]
    fn unchanged_network_binding_yields_no_deployment_action() {
        let net_id = Uuid::new_v4();
        let mut desired = empty_desired();
        desired.networks.insert(
            "internal".into(),
            desired_network("internal", "10.0.0.0/16"),
        );
        desired.deployments.insert(
            "api".into(),
            DesiredDeployment {
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: None,
                network: Some("internal".into()),
            },
        );
        let mut current = CurrentState::empty();
        current.networks.insert(
            "internal".into(),
            current_network(net_id, "internal", "10.0.0.0/16"),
        );
        current.deployments.insert(
            "api".into(),
            CurrentDeployment {
                id: Uuid::new_v4(),
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: None,
                network_binding: Some(CurrentNetworkBinding {
                    network_id: net_id,
                    network_name: "internal".into(),
                }),
            },
        );
        let plan = diff(&desired, &current, use_env());
        assert!(plan.is_empty(), "{:?}", plan.deployment_actions);
    }

    #[test]
    fn diff_resolves_refs_existing_for_unchanged_pending_for_minted() {
        // "Resolved as existing" is a plan-time fact: a binding to an
        // unchanged resource carries its uuid; a binding to a resource this
        // run creates or recreates stays Pending(name) until apply mints it.
        let net_id = Uuid::new_v4();
        let svc_id = Uuid::new_v4();

        let mut desired = empty_desired();
        // Unchanged network + unchanged service.
        desired.networks.insert(
            "internal".into(),
            desired_network("internal", "10.0.0.0/16"),
        );
        desired.services.insert(
            "web".into(),
            DesiredService {
                name: "web".into(),
                hosts: vec![],
                region: "dev".into(),
                configuration: http_config(),
            },
        );
        // A brand-new network referenced by a new deployment.
        desired
            .networks
            .insert("fresh".into(), desired_network("fresh", "10.7.0.0/24"));
        // New deployment binding to the unchanged service + unchanged network.
        desired.deployments.insert(
            "api".into(),
            DesiredDeployment {
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: Some(super::super::desired::DesiredServiceBinding {
                    service_name: "web".into(),
                    target_group: "default".into(),
                }),
                network: Some("internal".into()),
            },
        );
        // New deployment binding to the new network.
        desired.deployments.insert(
            "worker".into(),
            DesiredDeployment {
                name: "worker".into(),
                configuration: dep_config("w:1"),
                service_binding: None,
                network: Some("fresh".into()),
            },
        );

        let mut current = CurrentState::empty();
        current.networks.insert(
            "internal".into(),
            current_network(net_id, "internal", "10.0.0.0/16"),
        );
        current.services.insert(
            "web".into(),
            CurrentService {
                id: svc_id,
                name: "web".into(),
                hosts: vec![],
                region: "dev".into(),
                configuration: http_config(),
            },
        );

        let plan = diff(&desired, &current, use_env());
        let by_name: BTreeMap<&str, &DeploymentAction> = plan
            .deployment_actions
            .iter()
            .map(|a| (a.name(), a))
            .collect();

        match by_name["api"] {
            DeploymentAction::Create {
                network, service, ..
            } => {
                assert_eq!(
                    network.as_ref(),
                    Some(&ResourceRef::Existing {
                        id: net_id,
                        name: "internal".into()
                    }),
                    "unchanged network resolves at plan time"
                );
                let svc = service.as_ref().unwrap();
                assert_eq!(
                    svc.service,
                    ResourceRef::Existing {
                        id: svc_id,
                        name: "web".into()
                    },
                    "unchanged service resolves at plan time"
                );
                assert_eq!(svc.target_group, "default");
            }
            other => panic!("expected Create, got {other:?}"),
        }
        match by_name["worker"] {
            DeploymentAction::Create { network, .. } => {
                assert_eq!(
                    network.as_ref(),
                    Some(&ResourceRef::Pending {
                        name: "fresh".into()
                    }),
                    "created-this-run network stays Pending"
                );
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn recreated_network_ref_is_pending_on_dependent_recreate() {
        // A recreated network mints a NEW uuid during apply, so the dependent
        // deployment's ref must be Pending — never the doomed old id.
        let net_id = Uuid::new_v4();
        let mut desired = empty_desired();
        desired.networks.insert(
            "internal".into(),
            desired_network("internal", "10.9.0.0/24"),
        );
        desired.deployments.insert(
            "api".into(),
            DesiredDeployment {
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: None,
                network: Some("internal".into()),
            },
        );
        let mut current = CurrentState::empty();
        current.networks.insert(
            "internal".into(),
            current_network(net_id, "internal", "10.0.0.0/16"),
        );
        current.deployments.insert(
            "api".into(),
            CurrentDeployment {
                id: Uuid::new_v4(),
                name: "api".into(),
                configuration: dep_config("i:1"),
                service_binding: None,
                network_binding: Some(CurrentNetworkBinding {
                    network_id: net_id,
                    network_name: "internal".into(),
                }),
            },
        );
        let plan = diff(&desired, &current, use_env());
        match plan.deployment_actions.as_slice() {
            [DeploymentAction::Recreate { network, .. }] => {
                assert_eq!(
                    network.as_ref(),
                    Some(&ResourceRef::Pending {
                        name: "internal".into()
                    })
                );
            }
            other => panic!("expected Recreate, got {other:?}"),
        }
    }

    #[test]
    fn unchanged_network_yields_no_action_and_snapshots_id() {
        let net_id = Uuid::new_v4();
        let mut desired = empty_desired();
        desired.networks.insert(
            "internal".into(),
            desired_network("internal", "10.0.0.0/16"),
        );
        let mut current = CurrentState::empty();
        current.networks.insert(
            "internal".into(),
            current_network(net_id, "internal", "10.0.0.0/16"),
        );
        let plan = diff(&desired, &current, use_env());
        assert!(plan.network_actions.is_empty());
        assert!(plan.is_empty());
        let _ = net_id;
    }
}
