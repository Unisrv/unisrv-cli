//! Plan structure and pure diff function.
//!
//! `diff(desired, current)` produces a `Plan` value describing the create/update/
//! recreate/delete actions needed to converge `current` to `desired`. The function
//! is pure — no I/O — so it can be tested exhaustively without mocks.
//!
//! Important constraints from the backend:
//!  * `update_service` only accepts `HTTPServiceConfig`. `host` / `region` /
//!    `name` are immutable post-creation, so a change forces **Recreate**.
//!  * `update_deployment` only mutates `DeploymentConfiguration`. The
//!    `service_id` / `target_group` binding is creation-only, so a binding
//!    change forces **Recreate**.
//!  * Because `services -> deployments` is `ON DELETE SET NULL` and we cannot
//!    rebind, a service Recreate **cascade-recreates** every deployment bound
//!    to it.

use std::collections::{BTreeMap, BTreeSet};

use unisrv_api::models::{CreateEnvironmentRequest, DeploymentConfiguration, HTTPServiceConfig};
use uuid::Uuid;

use super::desired::{DesiredDeployment, DesiredService, DesiredState};

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentState {
    pub services: BTreeMap<String, CurrentService>,
    pub deployments: BTreeMap<String, CurrentDeployment>,
}

impl CurrentState {
    pub fn empty() -> Self {
        CurrentState {
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentService {
    pub id: Uuid,
    pub name: String,
    pub host: String,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentServiceBinding {
    pub service_id: Uuid,
    pub service_name: String,
    pub target_group: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub project: String,
    pub env_action: EnvAction,
    pub service_actions: Vec<ServiceAction>,
    pub deployment_actions: Vec<DeploymentAction>,
    /// Snapshot of existing service IDs by name, for apply to look up bindings
    /// to services that aren't being acted on (unchanged) or were recreated.
    pub existing_service_ids: BTreeMap<String, Uuid>,
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

#[derive(Debug, Clone, PartialEq)]
pub enum DeploymentAction {
    Create(DesiredDeployment),
    Update {
        id: Uuid,
        desired: DesiredDeployment,
        current: CurrentDeployment,
    },
    Recreate {
        current: CurrentDeployment,
        desired: DesiredDeployment,
        reasons: Vec<RecreateReason>,
    },
    Delete(CurrentDeployment),
}

impl DeploymentAction {
    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        match self {
            DeploymentAction::Create(d) => &d.name,
            DeploymentAction::Update { desired, .. } => &desired.name,
            DeploymentAction::Recreate { desired, .. } => &desired.name,
            DeploymentAction::Delete(c) => &c.name,
        }
    }
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
}

pub fn diff(desired: &DesiredState, current: &CurrentState, env_action: EnvAction) -> Plan {
    // ── Services ──
    let mut service_actions: Vec<ServiceAction> = Vec::new();
    let mut recreated_services: BTreeSet<String> = BTreeSet::new();

    let desired_service_names: BTreeSet<&String> = desired.services.keys().collect();
    let current_service_names: BTreeSet<&String> = current.services.keys().collect();

    for name in &desired_service_names {
        let desired_svc = &desired.services[*name];
        match current.services.get(*name) {
            None => {
                service_actions.push(ServiceAction::Create(desired_svc.clone()));
            }
            Some(current_svc) => {
                let immutable_diffs =
                    super::diff::service::immutable_diffs(desired_svc, current_svc);
                if !immutable_diffs.is_empty() {
                    recreated_services.insert((*name).clone());
                    service_actions.push(ServiceAction::Recreate {
                        current: current_svc.clone(),
                        desired: desired_svc.clone(),
                        reasons: immutable_diffs,
                    });
                } else if desired_svc.configuration != current_svc.configuration {
                    service_actions.push(ServiceAction::Update {
                        id: current_svc.id,
                        desired: desired_svc.clone(),
                        current: current_svc.clone(),
                    });
                }
            }
        }
    }

    for name in current_service_names.difference(&desired_service_names) {
        service_actions.push(ServiceAction::Delete(current.services[*name].clone()));
    }

    // ── Deployments ──
    let mut deployment_actions: Vec<DeploymentAction> = Vec::new();

    let desired_dep_names: BTreeSet<&String> = desired.deployments.keys().collect();
    let current_dep_names: BTreeSet<&String> = current.deployments.keys().collect();

    for name in &desired_dep_names {
        let desired_dep = &desired.deployments[*name];
        match current.deployments.get(*name) {
            None => {
                deployment_actions.push(DeploymentAction::Create(desired_dep.clone()));
            }
            Some(current_dep) => {
                let mut reasons = Vec::new();

                // Cascade: if bound to a service being recreated, force recreate.
                if let Some(b) = &desired_dep.service_binding {
                    if recreated_services.contains(&b.service_name) {
                        reasons.push(RecreateReason::DependentServiceRecreated {
                            service_name: b.service_name.clone(),
                        });
                    }
                }

                if !service_bindings_match(
                    desired_dep.service_binding.as_ref(),
                    current_dep.service_binding.as_ref(),
                ) {
                    reasons.push(RecreateReason::ServiceBindingChanged);
                }

                if !reasons.is_empty() {
                    deployment_actions.push(DeploymentAction::Recreate {
                        current: current_dep.clone(),
                        desired: desired_dep.clone(),
                        reasons,
                    });
                } else if desired_dep.configuration != current_dep.configuration {
                    deployment_actions.push(DeploymentAction::Update {
                        id: current_dep.id,
                        desired: desired_dep.clone(),
                        current: current_dep.clone(),
                    });
                }
            }
        }
    }

    for name in current_dep_names.difference(&desired_dep_names) {
        deployment_actions.push(DeploymentAction::Delete(current.deployments[*name].clone()));
    }

    let existing_service_ids = current
        .services
        .iter()
        .map(|(name, svc)| (name.clone(), svc.id))
        .collect();

    Plan {
        project: desired.project.clone(),
        env_action,
        service_actions,
        deployment_actions,
        existing_service_ids,
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

impl Plan {
    pub fn is_empty(&self) -> bool {
        matches!(self.env_action, EnvAction::Use(_))
            && self.service_actions.is_empty()
            && self.deployment_actions.is_empty()
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
            network: None,
            instance_port: Some(80),
        }
    }

    fn desired_with_service(name: &str, host: &str) -> DesiredState {
        let mut s = DesiredState {
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        s.services.insert(
            name.into(),
            DesiredService {
                name: name.into(),
                host: host.into(),
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
                host: host.into(),
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
    fn host_change_is_service_recreate() {
        let plan = diff(
            &desired_with_service("web", "new.example.com"),
            &current_with_service("web", "old.example.com"),
            use_env(),
        );
        match &plan.service_actions[0] {
            ServiceAction::Recreate { reasons, .. } => {
                assert!(matches!(
                    reasons.as_slice(),
                    [RecreateReason::ImmutableField { field: "host", .. }]
                ));
            }
            other => panic!("expected Recreate, got {other:?}"),
        }
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
                host: "h.example".into(),
                region: "dev".into(),
                configuration: http_config(),
            },
        );
        current.deployments.insert(
            "web".into(),
            CurrentDeployment {
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
    fn host_change_cascades_to_dependent_deployment_recreate() {
        let mut desired = desired_with_service("web", "new.example");
        desired.deployments.insert(
            "web".into(),
            DesiredDeployment {
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
                host: "old.example".into(),
                region: "dev".into(),
                configuration: http_config(),
            },
        );
        current.deployments.insert(
            "web".into(),
            CurrentDeployment {
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
                    host: format!("{n}.example"),
                    region: "dev".into(),
                    configuration: http_config(),
                },
            );
        }
        // Deployment is desired bound to service "b".
        desired.deployments.insert(
            "dep".into(),
            DesiredDeployment {
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
                    host: format!("{n}.example"),
                    region: "dev".into(),
                    configuration: http_config(),
                },
            );
        }
        current.deployments.insert(
            "dep".into(),
            CurrentDeployment {
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
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        desired.deployments.insert(
            "worker".into(),
            DesiredDeployment {
                name: "worker".into(),
                configuration: dep_config("worker:1"),
                service_binding: None,
            },
        );
        let plan = diff(&desired, &CurrentState::empty(), use_env());
        assert!(plan.service_actions.is_empty());
        assert!(matches!(
            plan.deployment_actions.as_slice(),
            [DeploymentAction::Create(d)] if d.name == "worker"
        ));
    }

    #[test]
    fn extra_deployment_is_delete() {
        let desired = DesiredState {
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        let mut current = CurrentState::empty();
        current.deployments.insert(
            "old-worker".into(),
            CurrentDeployment {
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
            ("recreate-svc", "new-recreate.example", http_config()),
        ] {
            desired.services.insert(
                name.into(),
                DesiredService {
                    name: name.into(),
                    host: host.into(),
                    region: "dev".into(),
                    configuration: cfg,
                },
            );
        }

        // create-dep: new (binds to the new create-svc).
        // update-dep: image bump, binding unchanged on stable-svc.
        // recreate-dep: binding flips from stable-svc to create-svc.
        desired.deployments.insert(
            "create-dep".into(),
            DesiredDeployment {
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
            ("recreate-svc", recreate_id, "old-recreate.example"), // host differs
            ("delete-svc", delete_id, "delete.example"),
        ] {
            current.services.insert(
                name.into(),
                CurrentService {
                    id,
                    name: name.into(),
                    host: host.into(),
                    region: "dev".into(),
                    configuration: http_config(),
                },
            );
        }

        current.deployments.insert(
            "update-dep".into(),
            CurrentDeployment {
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
            DeploymentAction::Create(_)
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

        // existing_service_ids snapshots every current service so apply can
        // resolve bindings for unchanged services.
        assert_eq!(
            plan.existing_service_ids.get("stable-svc"),
            Some(&stable_id)
        );
        assert_eq!(
            plan.existing_service_ids.get("recreate-svc"),
            Some(&recreate_id)
        );
    }
}
