use async_trait::async_trait;
use uuid::Uuid;

use crate::auth::{AuthSession, AuthStore, LoginResponse};
use crate::error::{ApiError, Result, extract_error_reason};
use crate::models::*;

pub const DEFAULT_API_HOST: &str = "https://api.unisrv.io";
pub const API_HOST_ENV: &str = "UNISRV_API_HOST";

#[async_trait]
pub trait ApiClient: Send + Sync {
    // ── Auth ──
    async fn login(&self, username: &str, password: &str) -> Result<()>;
    async fn access_token(&self) -> Result<String>;
    async fn auth_session(&self) -> Result<AuthSession>;

    // ── Environments ──
    async fn create_environment(
        &self,
        req: CreateEnvironmentRequest,
    ) -> Result<EnvironmentResponse>;
    async fn list_environments(&self) -> Result<EnvironmentListResponse>;
    async fn update_environment(
        &self,
        id: Uuid,
        req: UpdateEnvironmentRequest,
    ) -> Result<EnvironmentResponse>;
    async fn delete_environment(&self, id: Uuid) -> Result<()>;

    // ── Instances ──
    async fn provision_instance(
        &self,
        env_id: Uuid,
        req: InstanceProvisionRequest,
    ) -> Result<InstanceProvisionResponse>;
    async fn deprovision_instance(
        &self,
        env_id: Uuid,
        instance_id: Uuid,
        req: Option<InstanceDeprovisionRequest>,
    ) -> Result<()>;
    async fn get_instance(
        &self,
        env_id: Uuid,
        instance_id: Uuid,
        include_service_targets: bool,
        include_proxied_ports: bool,
    ) -> Result<InstanceDetailResponse>;
    async fn list_instances(&self, env_id: Uuid) -> Result<InstanceListResponse>;
    async fn get_instance_logs(&self, env_id: Uuid, instance_id: Uuid) -> Result<Vec<LogMessage>>;
    async fn create_tcp_proxy(
        &self,
        env_id: Uuid,
        instance_id: Uuid,
        req: CreateInstanceTCPProxyRequest,
    ) -> Result<CreateInstanceTCPProxyResponse>;

    // ── Networks ──
    async fn create_network(&self, req: CreateInternalNetworkRequest) -> Result<NetworkResponse>;
    async fn delete_network(&self, network_id: Uuid) -> Result<()>;
    async fn list_networks(&self, include_instance_count: bool) -> Result<NetworkListResponse>;
    async fn get_network(&self, network_id: Uuid) -> Result<NetworkResponse>;

    // ── Services ──
    async fn provision_service(
        &self,
        env_id: Uuid,
        req: ServiceProvisionRequest,
    ) -> Result<ServiceProvisionResponse>;
    async fn list_services(&self, env_id: Uuid) -> Result<ServiceListResponse>;
    async fn get_service(&self, env_id: Uuid, service_id: Uuid) -> Result<ServiceDetailResponse>;
    async fn update_service(
        &self,
        env_id: Uuid,
        service_id: Uuid,
        req: HTTPServiceConfig,
    ) -> Result<()>;
    async fn delete_service(&self, env_id: Uuid, service_id: Uuid) -> Result<()>;
    async fn create_service_target(
        &self,
        env_id: Uuid,
        service_id: Uuid,
        req: ServiceInstanceTarget,
    ) -> Result<CreateTargetResponse>;
    async fn delete_service_target(
        &self,
        env_id: Uuid,
        service_id: Uuid,
        target_id: Uuid,
    ) -> Result<()>;

    // ── Service Hosts ──
    async fn claim_host(&self, req: ClaimHostRequest) -> Result<HostResponse>;
    async fn list_hosts(&self) -> Result<Vec<HostResponse>>;
    async fn delete_host(&self, id: Uuid) -> Result<()>;
    async fn request_host_cert(&self, id: Uuid) -> Result<HostResponse>;
    async fn get_hosts_dns_config(&self) -> Result<DnsConfigResponse>;

    // ── Deployments ──
    async fn create_deployment(
        &self,
        env_id: Uuid,
        req: CreateDeploymentRequest,
    ) -> Result<CreateDeploymentResponse>;
    async fn list_deployments(&self, env_id: Uuid) -> Result<DeploymentListResponse>;
    async fn get_deployment(
        &self,
        env_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<DeploymentDetailResponse>;
    async fn update_deployment(
        &self,
        env_id: Uuid,
        deployment_id: Uuid,
        req: UpdateDeploymentRequest,
    ) -> Result<()>;
    async fn delete_deployment(&self, env_id: Uuid, deployment_id: Uuid) -> Result<()>;
}

pub struct HttpApiClient {
    client: reqwest::Client,
    base_url: String,
    auth_store: AuthStore,
    session: tokio::sync::RwLock<Option<AuthSession>>,
}

impl HttpApiClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let auth_store = AuthStore::new();
        let session = auth_store.load();

        HttpApiClient {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            auth_store,
            session: tokio::sync::RwLock::new(session),
        }
    }

    pub fn from_env() -> Self {
        let base_url = std::env::var(API_HOST_ENV).unwrap_or_else(|_| DEFAULT_API_HOST.to_string());
        Self::new(base_url)
    }

    pub(crate) async fn set_session(
        &self,
        session: AuthSession,
    ) -> std::result::Result<(), anyhow::Error> {
        self.auth_store.save(&session)?;
        *self.session.write().await = Some(session);
        Ok(())
    }

    pub async fn clear_session(&self) {
        self.auth_store.delete();
        *self.session.write().await = None;
    }

    pub async fn has_session(&self) -> bool {
        self.session.read().await.is_some()
    }

    async fn ensure_access_token(&self) -> Result<String> {
        // Fast path: token is still valid.
        {
            let guard = self.session.read().await;
            match guard.as_ref() {
                None => return Err(ApiError::not_logged_in()),
                Some(s) if s.expired() => {
                    return Err(ApiError::AuthRequired(
                        "Session expired. Please log in again.".into(),
                    ));
                }
                Some(s) if !s.access_token_expired() => {
                    return Ok(s.access_token().to_string());
                }
                _ => {}
            }
        }

        // Slow path: acquire write lock and re-check before refreshing.
        let mut guard = self.session.write().await;
        let session = guard.as_mut().ok_or_else(|| ApiError::not_logged_in())?;

        if session.access_token_expired() {
            session.refresh(&self.client, &self.base_url).await?;
            self.auth_store.save(session).map_err(ApiError::Other)?;
        }

        Ok(session.access_token().to_string())
    }

    async fn check_response(resp: reqwest::Response) -> Result<reqwest::Response> {
        let status = resp.status();
        if !status.is_success() {
            let reason = extract_error_reason(resp).await;
            return Err(ApiError::Server {
                status: status.as_u16(),
                reason,
            });
        }
        Ok(resp)
    }

    async fn send(&self, builder: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let token = self.ensure_access_token().await?;
        let resp = builder.bearer_auth(&token).send().await?;
        Self::check_response(resp).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(self
            .send(self.client.get(self.url(path)))
            .await?
            .json()
            .await?)
    }

    async fn post_for_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(self
            .send(self.client.post(self.url(path)))
            .await?
            .json()
            .await?)
    }

    async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        Ok(self
            .send(self.client.post(self.url(path)).json(body))
            .await?
            .json()
            .await?)
    }

    async fn put<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        Ok(self
            .send(self.client.put(self.url(path)).json(body))
            .await?
            .json()
            .await?)
    }

    async fn put_empty<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<()> {
        self.send(self.client.put(self.url(path)).json(body))
            .await?;
        Ok(())
    }

    async fn delete_req(&self, path: &str) -> Result<()> {
        self.send(self.client.delete(self.url(path))).await?;
        Ok(())
    }

    async fn delete_with_body<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<()> {
        self.send(self.client.delete(self.url(path)).json(body))
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ApiClient for HttpApiClient {
    // ── Auth ──

    async fn login(&self, username: &str, password: &str) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/auth/login/basic", self.base_url))
            .basic_auth(username, Some(password))
            .send()
            .await?;

        let resp = Self::check_response(resp).await?;
        let login_resp: LoginResponse = resp.json().await?;

        let session = AuthSession::from_login_response(login_resp);
        self.set_session(session).await.map_err(ApiError::Other)?;
        Ok(())
    }

    async fn access_token(&self) -> Result<String> {
        self.ensure_access_token().await
    }

    async fn auth_session(&self) -> Result<AuthSession> {
        self.ensure_access_token().await?;
        let guard = self.session.read().await;
        guard.clone().ok_or_else(|| ApiError::not_logged_in())
    }

    // ── Environments ──

    async fn create_environment(
        &self,
        req: CreateEnvironmentRequest,
    ) -> Result<EnvironmentResponse> {
        self.post("/environment", &req).await
    }

    async fn list_environments(&self) -> Result<EnvironmentListResponse> {
        self.get("/environments").await
    }

    async fn update_environment(
        &self,
        id: Uuid,
        req: UpdateEnvironmentRequest,
    ) -> Result<EnvironmentResponse> {
        self.put(&format!("/environment/{id}"), &req).await
    }

    async fn delete_environment(&self, id: Uuid) -> Result<()> {
        self.delete_req(&format!("/environment/{id}")).await
    }

    // ── Instances ──

    async fn provision_instance(
        &self,
        env_id: Uuid,
        req: InstanceProvisionRequest,
    ) -> Result<InstanceProvisionResponse> {
        self.post(&format!("/environment/{env_id}/instance"), &req)
            .await
    }

    async fn deprovision_instance(
        &self,
        env_id: Uuid,
        instance_id: Uuid,
        req: Option<InstanceDeprovisionRequest>,
    ) -> Result<()> {
        let path = format!("/environment/{env_id}/instance/{instance_id}");
        match req {
            Some(body) => self.delete_with_body(&path, &body).await,
            None => self.delete_req(&path).await,
        }
    }

    async fn get_instance(
        &self,
        env_id: Uuid,
        instance_id: Uuid,
        include_service_targets: bool,
        include_proxied_ports: bool,
    ) -> Result<InstanceDetailResponse> {
        let mut path = format!("/environment/{env_id}/instance/{instance_id}");
        let mut params = Vec::new();
        if include_service_targets {
            params.push("include_service_targets=true");
        }
        if include_proxied_ports {
            params.push("include_proxied_ports=true");
        }
        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }
        self.get(&path).await
    }

    async fn list_instances(&self, env_id: Uuid) -> Result<InstanceListResponse> {
        self.get(&format!("/environment/{env_id}/instances")).await
    }

    async fn get_instance_logs(&self, env_id: Uuid, instance_id: Uuid) -> Result<Vec<LogMessage>> {
        self.get(&format!(
            "/environment/{env_id}/instance/{instance_id}/logs"
        ))
        .await
    }

    async fn create_tcp_proxy(
        &self,
        env_id: Uuid,
        instance_id: Uuid,
        req: CreateInstanceTCPProxyRequest,
    ) -> Result<CreateInstanceTCPProxyResponse> {
        self.post(
            &format!("/environment/{env_id}/instance/{instance_id}/tcp"),
            &req,
        )
        .await
    }

    // ── Networks ──

    async fn create_network(&self, req: CreateInternalNetworkRequest) -> Result<NetworkResponse> {
        self.post("/network", &req).await
    }

    async fn delete_network(&self, network_id: Uuid) -> Result<()> {
        self.delete_req(&format!("/network/{network_id}")).await
    }

    async fn list_networks(&self, include_instance_count: bool) -> Result<NetworkListResponse> {
        let path = if include_instance_count {
            "/networks?include_instance_count=true".to_string()
        } else {
            "/networks".to_string()
        };
        self.get(&path).await
    }

    async fn get_network(&self, network_id: Uuid) -> Result<NetworkResponse> {
        self.get(&format!("/network/{network_id}")).await
    }

    // ── Services ──

    async fn provision_service(
        &self,
        env_id: Uuid,
        req: ServiceProvisionRequest,
    ) -> Result<ServiceProvisionResponse> {
        self.post(&format!("/environment/{env_id}/service"), &req)
            .await
    }

    async fn list_services(&self, env_id: Uuid) -> Result<ServiceListResponse> {
        self.get(&format!("/environment/{env_id}/services")).await
    }

    async fn get_service(&self, env_id: Uuid, service_id: Uuid) -> Result<ServiceDetailResponse> {
        self.get(&format!("/environment/{env_id}/service/{service_id}"))
            .await
    }

    async fn update_service(
        &self,
        env_id: Uuid,
        service_id: Uuid,
        req: HTTPServiceConfig,
    ) -> Result<()> {
        self.put_empty(&format!("/environment/{env_id}/service/{service_id}"), &req)
            .await
    }

    async fn delete_service(&self, env_id: Uuid, service_id: Uuid) -> Result<()> {
        self.delete_req(&format!("/environment/{env_id}/service/{service_id}"))
            .await
    }

    async fn create_service_target(
        &self,
        env_id: Uuid,
        service_id: Uuid,
        req: ServiceInstanceTarget,
    ) -> Result<CreateTargetResponse> {
        self.post(
            &format!("/environment/{env_id}/service/{service_id}/target"),
            &req,
        )
        .await
    }

    async fn delete_service_target(
        &self,
        env_id: Uuid,
        service_id: Uuid,
        target_id: Uuid,
    ) -> Result<()> {
        self.delete_req(&format!(
            "/environment/{env_id}/service/{service_id}/target/{target_id}"
        ))
        .await
    }

    // ── Service Hosts ──

    async fn claim_host(&self, req: ClaimHostRequest) -> Result<HostResponse> {
        self.post("/hosts", &req).await
    }

    async fn list_hosts(&self) -> Result<Vec<HostResponse>> {
        self.get("/hosts").await
    }

    async fn delete_host(&self, id: Uuid) -> Result<()> {
        self.delete_req(&format!("/hosts/{id}")).await
    }

    async fn request_host_cert(&self, id: Uuid) -> Result<HostResponse> {
        self.post_for_json(&format!("/hosts/{id}/cert")).await
    }

    async fn get_hosts_dns_config(&self) -> Result<DnsConfigResponse> {
        self.get("/hosts/dns-config").await
    }

    // ── Deployments ──

    async fn create_deployment(
        &self,
        env_id: Uuid,
        req: CreateDeploymentRequest,
    ) -> Result<CreateDeploymentResponse> {
        self.post(&format!("/environment/{env_id}/deployment"), &req)
            .await
    }

    async fn list_deployments(&self, env_id: Uuid) -> Result<DeploymentListResponse> {
        self.get(&format!("/environment/{env_id}/deployments"))
            .await
    }

    async fn get_deployment(
        &self,
        env_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<DeploymentDetailResponse> {
        self.get(&format!("/environment/{env_id}/deployment/{deployment_id}"))
            .await
    }

    async fn update_deployment(
        &self,
        env_id: Uuid,
        deployment_id: Uuid,
        req: UpdateDeploymentRequest,
    ) -> Result<()> {
        self.put_empty(
            &format!("/environment/{env_id}/deployment/{deployment_id}"),
            &req,
        )
        .await
    }

    async fn delete_deployment(&self, env_id: Uuid, deployment_id: Uuid) -> Result<()> {
        self.delete_req(&format!("/environment/{env_id}/deployment/{deployment_id}"))
            .await
    }
}
