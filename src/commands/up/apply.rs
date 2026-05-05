//! Execute a [`Plan`] against the API.
//!
//! Ordering rationale (see plan.rs for backend constraints):
//! 1. Create env (if EnvAction::Create).
//! 2. Create new services.
//! 3. Update services (config-only).
//! 4. Delete deployments being deleted *or recreated* (frees bindings).
//! 5. Recreate services: delete old, then create new (new IDs).
//! 6. Create deployments (new + recreated, looks up service_id by name).
//! 7. Update deployments (config-only).
//! 8. Delete services being fully removed.
//!
//! No rollback. On error, return immediately. Reconcile re-run will pick up.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use unisrv_api::ApiClient;
use unisrv_api::models::{
    CreateDeploymentRequest, DeploymentServiceBinding, ServiceProvisionRequest,
    UpdateDeploymentRequest,
};
use uuid::Uuid;

use super::desired::{DesiredDeployment, DesiredService, DesiredServiceBinding};
use super::plan::{DeploymentAction, EnvAction, Plan, ServiceAction};

pub async fn apply(plan: Plan, client: &dyn ApiClient) -> Result<()> {
    // ── Phase 1: env ──
    let env_id = match plan.env_action {
        EnvAction::Use(env) => env.id,
        EnvAction::Create(req) => {
            let env = client.create_environment(req.clone()).await.with_context(|| {
                format!("failed to create environment {:?}", req.name)
            })?;
            println!("  + environment {} created", env.name);
            env.id
        }
    };

    // service_ids: name → id, mutated as services are created/recreated.
    let mut service_ids: BTreeMap<String, Uuid> = plan.existing_service_ids.clone();

    // Partition service actions by phase.
    let (mut creates, mut updates, mut recreates, mut deletes): (Vec<_>, Vec<_>, Vec<_>, Vec<_>) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for action in plan.service_actions {
        match action {
            ServiceAction::Create(d) => creates.push(d),
            ServiceAction::Update { id, desired, .. } => updates.push((id, desired)),
            ServiceAction::Recreate { current, desired, .. } => recreates.push((current, desired)),
            ServiceAction::Delete(c) => deletes.push(c),
        }
    }

    // Partition deployment actions.
    let (mut dep_creates, mut dep_updates, mut dep_recreates, mut dep_deletes): (
        Vec<_>,
        Vec<_>,
        Vec<_>,
        Vec<_>,
    ) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for action in plan.deployment_actions {
        match action {
            DeploymentAction::Create(d) => dep_creates.push(d),
            DeploymentAction::Update { id, desired, .. } => dep_updates.push((id, desired)),
            DeploymentAction::Recreate { current, desired, .. } => {
                dep_recreates.push((current, desired))
            }
            DeploymentAction::Delete(c) => dep_deletes.push(c),
        }
    }

    // ── Phase 2: create new services ──
    for desired in creates {
        let id = create_service(client, env_id, &desired).await?;
        service_ids.insert(desired.name.clone(), id);
        println!("  + service {} created", desired.name);
    }

    // ── Phase 3: update services ──
    for (id, desired) in updates {
        client
            .update_service(env_id, id, desired.configuration.clone())
            .await
            .with_context(|| format!("failed to update service {:?}", desired.name))?;
        println!("  ~ service {} updated", desired.name);
    }

    // ── Phase 4: delete deployments being removed or recreated ──
    let to_delete_deps: Vec<_> = dep_deletes
        .iter()
        .map(|d| (d.id, d.name.clone()))
        .chain(dep_recreates.iter().map(|(c, _)| (c.id, c.name.clone())))
        .collect();
    for (id, name) in to_delete_deps {
        client
            .delete_deployment(env_id, id)
            .await
            .with_context(|| format!("failed to delete deployment {name:?}"))?;
        println!("  - deployment {name} deleted");
    }

    // ── Phase 5: recreate services (delete then create) ──
    for (current, desired) in recreates {
        client
            .delete_service(env_id, current.id)
            .await
            .with_context(|| format!("failed to delete service {:?}", current.name))?;
        let new_id = create_service(client, env_id, &desired).await?;
        service_ids.insert(desired.name.clone(), new_id);
        println!("  -/+ service {} recreated", desired.name);
    }

    // ── Phase 6: create deployments (new + recreated) ──
    let to_create_deps: Vec<DesiredDeployment> = dep_creates
        .into_iter()
        .chain(dep_recreates.into_iter().map(|(_c, d)| d))
        .collect();
    for desired in to_create_deps {
        create_deployment(client, env_id, &desired, &service_ids).await?;
        println!("  + deployment {} created", desired.name);
    }

    // ── Phase 7: update deployments ──
    for (id, desired) in dep_updates {
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
        println!("  ~ deployment {} updated", desired.name);
    }

    // ── Phase 8: delete services being removed ──
    for current in deletes {
        client
            .delete_service(env_id, current.id)
            .await
            .with_context(|| format!("failed to delete service {:?}", current.name))?;
        println!("  - service {} deleted", current.name);
    }

    Ok(())
}

async fn create_service(
    client: &dyn ApiClient,
    env_id: Uuid,
    desired: &DesiredService,
) -> Result<Uuid> {
    let req = ServiceProvisionRequest {
        region: desired.region.clone(),
        name: desired.name.clone(),
        host: desired.host.clone(),
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
    let id = service_ids.get(&binding.service_name).copied().ok_or_else(|| {
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
        CurrentDeployment, CurrentService, CurrentServiceBinding, EnvAction, Plan, RecreateReason,
        ServiceAction,
    };
    use chrono::NaiveDateTime;
    use unisrv_api::models::{
        CreateDeploymentResponse, DeploymentConfiguration, EnvironmentResponse, HTTPLocation,
        HTTPLocationTarget, HTTPServiceConfig, ServiceProvisionResponse,
    };
    use unisrv_api::test_support::MockApiClient;

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
        EnvAction::Use(EnvironmentResponse {
            id: Uuid::new_v4(),
            project: "demo".into(),
            name: "prod".into(),
            display_name: None,
            description: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
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
                display_name: None,
                description: None,
                created_at: NaiveDateTime::default(),
                updated_at: NaiveDateTime::default(),
            }))
            .with_provision_service(Ok(ServiceProvisionResponse {
                service_id: svc_id,
                connection_string: "n/a".into(),
            }))
            .with_create_deployment(Ok(CreateDeploymentResponse { id: dep_id }));

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
                host: "web.example".into(),
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
        };

        apply(plan, &client).await.unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.create_environment_calls.len(), 1);
        assert_eq!(calls.provision_service_calls.len(), 1);
        let (env_for_service, service_req) = &calls.provision_service_calls[0];
        assert_eq!(*env_for_service, new_env_id);
        assert_eq!(service_req.name, "web");
        assert_eq!(service_req.host, "web.example");

        assert_eq!(calls.create_deployment_calls.len(), 1);
        let (env_for_dep, dep_req) = &calls.create_deployment_calls[0];
        assert_eq!(*env_for_dep, new_env_id);
        let binding = dep_req.service.as_ref().unwrap();
        assert_eq!(binding.service_id, svc_id);
        assert_eq!(binding.target_group, "default");
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
                    host: "web.example".into(),
                    region: "dev".into(),
                    configuration: new_cfg,
                },
                current: CurrentService {
                    id: svc_id,
                    name: "web".into(),
                    host: "web.example".into(),
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
        };

        apply(plan, &client).await.unwrap();

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
            .with_provision_service(Ok(ServiceProvisionResponse {
                service_id: new_svc_id,
                connection_string: "x".into(),
            }))
            .with_create_deployment(Ok(CreateDeploymentResponse { id: new_dep_id }));

        let mut existing = BTreeMap::new();
        existing.insert("web".to_string(), old_svc_id);

        let plan = Plan {
            project: "demo".into(),
            env_action: use_env(),
            service_actions: vec![ServiceAction::Recreate {
                current: CurrentService {
                    id: old_svc_id,
                    name: "web".into(),
                    host: "old.example".into(),
                    region: "dev".into(),
                    configuration: http_config(),
                },
                desired: DesiredService {
                    name: "web".into(),
                    host: "new.example".into(),
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
        };

        apply(plan, &client).await.unwrap();

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
                host: "old.example".into(),
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
        };

        apply(plan, &client).await.unwrap();

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.delete_deployment_calls.len(), 1);
        assert_eq!(calls.delete_service_calls.len(), 1);
    }

    #[tokio::test]
    async fn deployment_create_without_binding_works() {
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_create_deployment(Ok(CreateDeploymentResponse { id: dep_id }));

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
        };

        apply(plan, &client).await.unwrap();

        let calls = client.calls.lock().unwrap();
        let (_env, req) = &calls.create_deployment_calls[0];
        assert!(req.service.is_none());
    }
}
