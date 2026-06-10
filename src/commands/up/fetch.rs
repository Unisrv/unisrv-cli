//! Fetch [`CurrentState`] from the API for a given environment.
//!
//! Custom hosts come straight from each `ServiceDetailResponse.custom_hosts`
//! (the authoritative per-service set). `region` is not exposed by the backend
//! yet, so it's treated as the configured default — switch to the response
//! value when the backend exposes it.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use unisrv_api::ApiClient;
use unisrv_api::models::HTTPServiceConfig;
use uuid::Uuid;

use super::defaults::DEFAULT_REGION;
use super::plan::{
    CurrentDeployment, CurrentNetwork, CurrentNetworkBinding, CurrentService,
    CurrentServiceBinding, CurrentState,
};

pub async fn fetch_current_state(client: &dyn ApiClient, env_id: Uuid) -> Result<CurrentState> {
    let networks_list = client
        .list_networks(env_id, false)
        .await
        .context("failed to list networks")?;

    let mut networks_by_id: BTreeMap<Uuid, CurrentNetwork> = BTreeMap::new();
    let mut networks: BTreeMap<String, CurrentNetwork> = BTreeMap::new();
    for entry in networks_list.networks {
        let net = CurrentNetwork {
            id: entry.id,
            name: entry.name.clone(),
            ipv4_cidr: entry.ipv4_cidr,
        };
        networks_by_id.insert(entry.id, net.clone());
        networks.insert(entry.name, net);
    }

    let services_list = client.list_services(env_id).await?;

    let mut services_by_id: BTreeMap<Uuid, CurrentService> = BTreeMap::new();
    let mut services: BTreeMap<String, CurrentService> = BTreeMap::new();
    for entry in services_list.services {
        let detail = client.get_service(env_id, entry.id).await?;
        let configuration: HTTPServiceConfig = serde_json::from_value(detail.configuration.clone())
            .with_context(|| format!("failed to parse configuration for service {}", entry.name))?;
        let svc = CurrentService {
            id: entry.id,
            name: detail.name.clone(),
            hosts: detail.custom_hosts.clone(),
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
        // A dangling network_id (network deleted out-of-band) resolves to no
        // binding — the deployment is effectively detached.
        let network_binding = detail.network_id.and_then(|nid| {
            networks_by_id.get(&nid).map(|net| CurrentNetworkBinding {
                network_id: nid,
                network_name: net.name.clone(),
            })
        });
        deployments.insert(
            detail.name.clone(),
            CurrentDeployment {
                id: entry.id,
                name: detail.name,
                configuration: detail.configuration,
                service_binding,
                network_binding,
            },
        );
    }

    Ok(CurrentState {
        services,
        deployments,
        networks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use serde_json::json;
    use unisrv_api::models::{
        DeploymentConfiguration, DeploymentDetailResponse, DeploymentListEntry,
        DeploymentListResponse, DeploymentState, HTTPLocationTarget, NetworkListItem,
        NetworkListResponse, ServiceDetailResponse, ServiceListItem, ServiceListResponse,
    };
    use unisrv_api::test_support::MockApiClient;

    fn service_detail(id: Uuid, env_id: Uuid, name: &str) -> ServiceDetailResponse {
        ServiceDetailResponse {
            id,
            name: name.into(),
            base_host: format!("{name}-env.unisrv.dev"),
            custom_hosts: vec![],
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
            network_id: None,
            instances: vec![],
            backoff: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
        }
    }

    #[tokio::test]
    async fn fetches_empty_state() {
        let env = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_list_networks(Ok(NetworkListResponse { networks: vec![] }))
            .with_list_services(Ok(ServiceListResponse { services: vec![] }))
            .with_list_deployments(Ok(DeploymentListResponse {
                deployments: vec![],
            }));
        let state = fetch_current_state(&client, env).await.unwrap();
        assert!(state.services.is_empty());
        assert!(state.deployments.is_empty());
    }

    #[tokio::test]
    async fn fetches_service_hosts_from_detail_custom_hosts() {
        // Current custom hosts are the authoritative per-service set from the
        // service detail — NOT derived from the global list_hosts(). Here
        // list_hosts is empty, yet the service still reports its custom host.
        let env = Uuid::new_v4();
        let svc_id = Uuid::new_v4();
        let mut detail = service_detail(svc_id, env, "web");
        detail.custom_hosts = vec!["shop.acme.com".into()];
        let client = MockApiClient::logged_in()
            .with_list_networks(Ok(NetworkListResponse { networks: vec![] }))
            .with_list_services(Ok(ServiceListResponse {
                services: vec![ServiceListItem {
                    id: svc_id,
                    name: "web".into(),
                    base_host: "web-env.unisrv.dev".into(),
                    custom_hosts: vec!["shop.acme.com".into()],
                }],
            }))
            .push_get_service(Ok(detail))
            .with_list_deployments(Ok(DeploymentListResponse {
                deployments: vec![],
            }));
        let state = fetch_current_state(&client, env).await.unwrap();
        let svc = &state.services["web"];
        assert_eq!(svc.hosts, vec!["shop.acme.com".to_string()]);
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
            .with_list_networks(Ok(NetworkListResponse { networks: vec![] }))
            .with_list_services(Ok(ServiceListResponse {
                services: vec![ServiceListItem {
                    id: svc_id,
                    name: "web".into(),
                    base_host: "web-env.unisrv.dev".into(),
                    custom_hosts: vec![],
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
        let state = fetch_current_state(&client, env).await.unwrap();
        let dep = &state.deployments["web"];
        let binding = dep.service_binding.as_ref().unwrap();
        assert_eq!(binding.service_name, "web");
        assert_eq!(binding.target_group, "default");
    }

    #[tokio::test]
    async fn fetches_networks_and_resolves_deployment_network_binding() {
        let env = Uuid::new_v4();
        let net_id = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let mut detail = deployment_detail(dep_id, "api", None, None);
        detail.network_id = Some(net_id);
        let client = MockApiClient::logged_in()
            .with_list_services(Ok(ServiceListResponse { services: vec![] }))
            .with_list_networks(Ok(NetworkListResponse {
                networks: vec![NetworkListItem {
                    id: net_id,
                    name: "internal".into(),
                    ipv4_cidr: "10.0.0.0/16".into(),
                    instance_count: None,
                }],
            }))
            .with_list_deployments(Ok(DeploymentListResponse {
                deployments: vec![DeploymentListEntry {
                    id: dep_id,
                    name: "api".into(),
                    state: DeploymentState("running".into()),
                    replicas: 1,
                    container_image: "i:1".into(),
                    created_at: NaiveDateTime::default(),
                }],
            }))
            .push_get_deployment(Ok(detail));

        let state = fetch_current_state(&client, env).await.unwrap();

        let net = &state.networks["internal"];
        assert_eq!(net.id, net_id);
        assert_eq!(net.ipv4_cidr, "10.0.0.0/16");

        let binding = state.deployments["api"].network_binding.as_ref().unwrap();
        assert_eq!(binding.network_id, net_id);
        assert_eq!(binding.network_name, "internal");
    }

    #[tokio::test]
    async fn list_networks_failure_is_contextualized() {
        // The networks listing is the first call of every fetch; a bare API
        // error with no framing would be the whole story the user sees.
        let env = Uuid::new_v4();
        let client =
            MockApiClient::logged_in().with_list_networks(Err(unisrv_api::ApiError::Server {
                status: 500,
                reason: "boom".into(),
            }));
        let err = fetch_current_state(&client, env).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed to list networks"),
            "frames the failing step: {msg}"
        );
    }

    #[tokio::test]
    async fn dangling_network_id_resolves_to_no_binding() {
        // A deployment can reference a network that no longer exists (deleted
        // out-of-band; the FK is ON DELETE SET NULL but a race is possible).
        // That must surface as "no binding", not an error.
        let env = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let mut detail = deployment_detail(dep_id, "api", None, None);
        detail.network_id = Some(Uuid::new_v4()); // not in list_networks
        let client = MockApiClient::logged_in()
            .with_list_services(Ok(ServiceListResponse { services: vec![] }))
            .with_list_networks(Ok(NetworkListResponse { networks: vec![] }))
            .with_list_deployments(Ok(DeploymentListResponse {
                deployments: vec![DeploymentListEntry {
                    id: dep_id,
                    name: "api".into(),
                    state: DeploymentState("running".into()),
                    replicas: 1,
                    container_image: "i:1".into(),
                    created_at: NaiveDateTime::default(),
                }],
            }))
            .push_get_deployment(Ok(detail));

        let state = fetch_current_state(&client, env).await.unwrap();
        assert!(state.deployments["api"].network_binding.is_none());
    }

    #[tokio::test]
    async fn deployment_without_binding_returns_none() {
        let env = Uuid::new_v4();
        let dep_id = Uuid::new_v4();
        let client = MockApiClient::logged_in()
            .with_list_networks(Ok(NetworkListResponse { networks: vec![] }))
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
        let state = fetch_current_state(&client, env).await.unwrap();
        assert!(state.deployments["worker"].service_binding.is_none());
    }
}
