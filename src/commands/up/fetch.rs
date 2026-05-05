//! Fetch [`CurrentState`] from the API for a given environment.
//!
//! NOTE: the backend's `ServiceDetailResponse` does not expose `host` or
//! `region`. We work around that by deriving `host` from `list_hosts()`
//! (each `HostResponse` has `service_id` once attached). `region` is
//! treated as the configured default — when the backend exposes it, switch
//! to using the response value.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use unisrv_api::ApiClient;
use unisrv_api::models::{HTTPServiceConfig, HostResponse};
use uuid::Uuid;

use super::defaults::DEFAULT_REGION;
use super::plan::{CurrentDeployment, CurrentService, CurrentServiceBinding, CurrentState};

pub async fn fetch_current_state(
    client: &dyn ApiClient,
    env_id: Uuid,
    hosts: &[HostResponse],
) -> Result<CurrentState> {
    let host_by_service: BTreeMap<Uuid, &HostResponse> = hosts
        .iter()
        .filter_map(|h| h.service_id.map(|sid| (sid, h)))
        .collect();

    let services_list = client.list_services(env_id).await?;

    let mut services_by_id: BTreeMap<Uuid, CurrentService> = BTreeMap::new();
    let mut services: BTreeMap<String, CurrentService> = BTreeMap::new();
    for entry in services_list.services {
        let detail = client.get_service(env_id, entry.id).await?;
        let configuration: HTTPServiceConfig = serde_json::from_value(detail.configuration.clone())
            .with_context(|| format!("failed to parse configuration for service {}", entry.name))?;
        let host = host_by_service
            .get(&entry.id)
            .map(|h| h.host.clone())
            .unwrap_or_default();
        let svc = CurrentService {
            id: entry.id,
            name: detail.name.clone(),
            host,
            region: DEFAULT_REGION.to_string(),
            configuration,
        };
        services_by_id.insert(entry.id, svc.clone());
        services.insert(detail.name, svc);
    }

    let deployments_list = client.list_deployments(env_id).await?;

    let mut deployments: BTreeMap<String, CurrentDeployment> = BTreeMap::new();
    for entry in deployments_list.deployments {
        let detail = client.get_deployment(env_id, entry.id).await?;
        let service_binding = match (detail.service_id, detail.service_target_group.as_ref()) {
            (Some(sid), Some(tg)) => services_by_id.get(&sid).map(|svc| CurrentServiceBinding {
                service_id: sid,
                service_name: svc.name.clone(),
                target_group: tg.clone(),
            }),
            _ => None,
        };
        deployments.insert(
            detail.name.clone(),
            CurrentDeployment {
                id: entry.id,
                name: detail.name,
                configuration: detail.configuration,
                service_binding,
            },
        );
    }

    Ok(CurrentState {
        services,
        deployments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use serde_json::json;
    use unisrv_api::models::{
        DeploymentConfiguration, DeploymentDetailResponse, DeploymentListEntry,
        DeploymentListResponse, DeploymentState, HTTPLocation, HTTPLocationTarget,
        ServiceDetailResponse, ServiceListItem, ServiceListResponse,
    };
    use unisrv_api::test_support::MockApiClient;

    fn host(host_str: &str, service_id: Uuid) -> HostResponse {
        HostResponse {
            id: Uuid::new_v4(),
            host: host_str.to_string(),
            user_id: Uuid::new_v4(),
            service_id: Some(service_id),
            certificate_type: Some("le".into()),
            certificate_valid_until: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
        }
    }

    fn service_detail(id: Uuid, env_id: Uuid, name: &str) -> ServiceDetailResponse {
        ServiceDetailResponse {
            id,
            name: name.into(),
            configuration: json!({
                "locations": [{
                    "path": "/",
                    "target": { "type": "instance", "group": "default" }
                }],
                "allow_http": false,
            }),
            environment_id: env_id,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
            providers: vec![],
            targets: vec![],
            statistics: None,
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

    fn deployment_detail(
        id: Uuid,
        name: &str,
        service_id: Option<Uuid>,
        target_group: Option<&str>,
    ) -> DeploymentDetailResponse {
        DeploymentDetailResponse {
            id,
            name: name.into(),
            state: DeploymentState("running".into()),
            configuration: dep_config("nginx:1"),
            metadata: json!({}),
            service_id,
            service_target_group: target_group.map(String::from),
            instances: vec![],
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
        }
    }

    #[tokio::test]
    async fn fetches_empty_state() {
        let env = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_list_services(Ok(ServiceListResponse { services: vec![] }))
            .with_list_deployments(Ok(DeploymentListResponse {
                deployments: vec![],
            }));
        let state = fetch_current_state(&client, env, &[]).await.unwrap();
        assert!(state.services.is_empty());
        assert!(state.deployments.is_empty());
    }

    #[tokio::test]
    async fn fetches_service_with_host_from_host_claims() {
        let env = Uuid::new_v4();
        let svc_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_list_services(Ok(ServiceListResponse {
                services: vec![ServiceListItem {
                    id: svc_id,
                    name: "web".into(),
                }],
            }))
            .push_get_service(Ok(service_detail(svc_id, env, "web")))
            .with_list_deployments(Ok(DeploymentListResponse {
                deployments: vec![],
            }));
        let hosts = vec![host("web.example", svc_id)];
        let state = fetch_current_state(&client, env, &hosts).await.unwrap();
        let svc = &state.services["web"];
        assert_eq!(svc.host, "web.example");
        assert_eq!(svc.region, "dev");
        assert_eq!(svc.configuration.allow_http, false);
        assert_eq!(svc.configuration.locations.len(), 1);
        match &svc.configuration.locations[0].target {
            HTTPLocationTarget::Instance { group } => assert_eq!(group, "default"),
            _ => panic!("unexpected"),
        }
    }

    #[tokio::test]
    async fn fetches_deployment_with_resolved_service_binding() {
        let env = Uuid::new_v4();
        let svc_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_list_services(Ok(ServiceListResponse {
                services: vec![ServiceListItem {
                    id: svc_id,
                    name: "web".into(),
                }],
            }))
            .push_get_service(Ok(service_detail(svc_id, env, "web")))
            .with_list_deployments(Ok(DeploymentListResponse {
                deployments: vec![DeploymentListEntry {
                    id: dep_id,
                    name: "web".into(),
                    state: DeploymentState("running".into()),
                    replicas: 1,
                    container_image: "nginx:1".into(),
                    created_at: NaiveDateTime::default(),
                }],
            }))
            .push_get_deployment(Ok(deployment_detail(
                dep_id,
                "web",
                Some(svc_id),
                Some("default"),
            )));
        let state = fetch_current_state(&client, env, &[]).await.unwrap();
        let dep = &state.deployments["web"];
        let binding = dep.service_binding.as_ref().unwrap();
        assert_eq!(binding.service_name, "web");
        assert_eq!(binding.target_group, "default");
    }

    #[tokio::test]
    async fn deployment_without_binding_returns_none() {
        let env = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_list_services(Ok(ServiceListResponse { services: vec![] }))
            .with_list_deployments(Ok(DeploymentListResponse {
                deployments: vec![DeploymentListEntry {
                    id: dep_id,
                    name: "worker".into(),
                    state: DeploymentState("running".into()),
                    replicas: 1,
                    container_image: "w:1".into(),
                    created_at: NaiveDateTime::default(),
                }],
            }))
            .push_get_deployment(Ok(deployment_detail(dep_id, "worker", None, None)));
        let state = fetch_current_state(&client, env, &[]).await.unwrap();
        assert!(state.deployments["worker"].service_binding.is_none());
    }
}
