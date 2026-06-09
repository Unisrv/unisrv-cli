use async_trait::async_trait;
use futures_util::stream::BoxStream;
use uuid::Uuid;

use crate::auth::{AuthSession, AuthStore, LoginResponse};
use crate::error::{ApiError, Result, extract_error_reason};
use crate::models::*;

pub const DEFAULT_API_HOST: &str = "https://api.unisrv.io";
pub const API_HOST_ENV: &str = "UNISRV_API_HOST";

/// A live stream of log frames. Each item is one parsed [`LogMessage`], or an
/// error if a frame failed to parse or the transport broke. The stream ends
/// when the server closes the connection (e.g. the instance stopped).
pub type LogStream = BoxStream<'static, Result<LogMessage>>;

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
    /// Open a live log stream for an instance. The server replays the existing
    /// log history, then follows new frames until the connection closes.
    async fn stream_instance_logs(&self, env_id: Uuid, instance_id: Uuid) -> Result<LogStream>;
    async fn create_tcp_proxy(
        &self,
        env_id: Uuid,
        instance_id: Uuid,
        req: CreateInstanceTCPProxyRequest,
    ) -> Result<CreateInstanceTCPProxyResponse>;

    // ── Networks ──
    async fn create_network(
        &self,
        env_id: Uuid,
        req: CreateInternalNetworkRequest,
    ) -> Result<NetworkResponse>;
    async fn delete_network(&self, env_id: Uuid, network_id: Uuid) -> Result<()>;
    async fn list_networks(
        &self,
        env_id: Uuid,
        include_instance_count: bool,
    ) -> Result<NetworkListResponse>;
    async fn get_network(&self, env_id: Uuid, network_id: Uuid) -> Result<NetworkResponse>;

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
    /// Link a claimed host to a service (PUT /hosts/{id}/service/{service_id}).
    async fn link_host_to_service(&self, id: Uuid, service_id: Uuid) -> Result<HostResponse>;
    /// Unlink a host from a service (DELETE /hosts/{id}/service/{service_id}).
    async fn unlink_host_from_service(&self, id: Uuid, service_id: Uuid) -> Result<HostResponse>;

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

    // ── Container Registries ──
    async fn create_registry(
        &self,
        req: CreateRegistryRequest,
        validate: bool,
    ) -> Result<RegistryResponse>;
    async fn list_registries(&self) -> Result<RegistryListResponse>;
    async fn update_registry(
        &self,
        id: Uuid,
        req: UpdateRegistryRequest,
        validate: bool,
    ) -> Result<RegistryResponse>;
    async fn delete_registry(&self, id: Uuid) -> Result<()>;
    async fn test_registry(&self, id: Uuid) -> Result<TestRegistryResponse>;
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

    /// PUT with no request body, parsing the JSON response.
    async fn put_for_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(self
            .send(self.client.put(self.url(path)))
            .await?
            .json()
            .await?)
    }

    /// DELETE with no request body, parsing the JSON response.
    async fn delete_for_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(self
            .send(self.client.delete(self.url(path)))
            .await?
            .json()
            .await?)
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

    async fn stream_instance_logs(&self, env_id: Uuid, instance_id: Uuid) -> Result<LogStream> {
        use futures_util::StreamExt;
        use reqwest_websocket::RequestBuilderExt;

        // The upgrade request carries auth like any other call, but bypasses the
        // JSON `send`/`check_response` helpers since the response is a 101 switch.
        let token = self.ensure_access_token().await?;
        let url = self.url(&format!(
            "/environment/{env_id}/instance/{instance_id}/logs/stream"
        ));
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .upgrade()
            .send()
            .await
            .map_err(|e| ApiError::Other(anyhow::anyhow!("failed to open log stream: {e}")))?;
        // A non-101 response (401/403/404, …) surfaces here as a handshake error;
        // translate the status into a clear message instead of a generic upgrade
        // failure, since the WS path bypasses the JSON `check_response` helper.
        let websocket = response.into_websocket().await.map_err(map_upgrade_error)?;

        // Classify each frame: text → parsed log, abnormal close → error (so a
        // server-side failure isn't reported as a clean end), transport break →
        // error. A normal close ends the stream cleanly.
        let stream = websocket.filter_map(|message| async move {
            match message {
                Ok(frame) => classify_frame(frame),
                Err(e) => Some(Err(ApiError::Other(anyhow::anyhow!(
                    "log stream error: {e}"
                )))),
            }
        });

        Ok(stream.boxed())
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

    async fn create_network(
        &self,
        env_id: Uuid,
        req: CreateInternalNetworkRequest,
    ) -> Result<NetworkResponse> {
        self.post(&format!("/environment/{env_id}/network"), &req)
            .await
    }

    async fn delete_network(&self, env_id: Uuid, network_id: Uuid) -> Result<()> {
        self.delete_req(&format!("/environment/{env_id}/network/{network_id}"))
            .await
    }

    async fn list_networks(
        &self,
        env_id: Uuid,
        include_instance_count: bool,
    ) -> Result<NetworkListResponse> {
        let path = if include_instance_count {
            format!("/environment/{env_id}/networks?include_instance_count=true")
        } else {
            format!("/environment/{env_id}/networks")
        };
        self.get(&path).await
    }

    async fn get_network(&self, env_id: Uuid, network_id: Uuid) -> Result<NetworkResponse> {
        self.get(&format!("/environment/{env_id}/network/{network_id}"))
            .await
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

    async fn link_host_to_service(&self, id: Uuid, service_id: Uuid) -> Result<HostResponse> {
        self.put_for_json(&format!("/hosts/{id}/service/{service_id}"))
            .await
    }

    async fn unlink_host_from_service(&self, id: Uuid, service_id: Uuid) -> Result<HostResponse> {
        self.delete_for_json(&format!("/hosts/{id}/service/{service_id}"))
            .await
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

    // ── Container Registries ──

    async fn create_registry(
        &self,
        req: CreateRegistryRequest,
        validate: bool,
    ) -> Result<RegistryResponse> {
        let path = registries_path_with_validate("/registries", validate);
        self.post(&path, &req).await
    }

    async fn list_registries(&self) -> Result<RegistryListResponse> {
        self.get("/registries").await
    }

    async fn update_registry(
        &self,
        id: Uuid,
        req: UpdateRegistryRequest,
        validate: bool,
    ) -> Result<RegistryResponse> {
        let path = registries_path_with_validate(&format!("/registries/{id}"), validate);
        self.put(&path, &req).await
    }

    async fn delete_registry(&self, id: Uuid) -> Result<()> {
        self.delete_req(&format!("/registries/{id}")).await
    }

    async fn test_registry(&self, id: Uuid) -> Result<TestRegistryResponse> {
        self.post_for_json(&format!("/registries/{id}/test")).await
    }
}

fn registries_path_with_validate(base: &str, validate: bool) -> String {
    if validate {
        format!("{base}?validate=true")
    } else {
        base.to_string()
    }
}

/// Turn one WebSocket frame into a log-stream item.
///
/// Text frames carry the log JSON. A *normal* close ends the stream cleanly
/// (`None`). An *abnormal* close becomes an error so a server-side failure isn't
/// silently reported as a successful end of follow. All other control/binary
/// frames carry nothing to show and are ignored.
fn classify_frame(frame: reqwest_websocket::Message) -> Option<Result<LogMessage>> {
    use reqwest_websocket::{CloseCode, Message};
    match frame {
        Message::Text(text) => {
            Some(serde_json::from_str::<LogMessage>(&text).map_err(ApiError::from))
        }
        Message::Close { code, reason } if code != CloseCode::Normal => Some(Err(ApiError::Other(
            anyhow::anyhow!("log stream closed abnormally ({code}): {reason}"),
        ))),
        _ => None,
    }
}

/// Map a failed WebSocket upgrade onto a meaningful error. A non-101 status is
/// the common real failure (expired session, missing instance); surface its
/// class rather than a generic "failed to upgrade". The server's response body
/// is already consumed by the handshake, so only the status is available.
fn map_upgrade_error(e: reqwest_websocket::Error) -> ApiError {
    use reqwest_websocket::{Error, HandshakeError};
    if let Error::Handshake(HandshakeError::UnexpectedStatusCode(status)) = &e {
        let code = status.as_u16();
        return match code {
            401 | 403 => ApiError::AuthRequired(
                "not authorized to stream logs; your session may have expired — log in again"
                    .into(),
            ),
            404 => ApiError::Server {
                status: code,
                reason: "instance not found".into(),
            },
            _ => ApiError::Server {
                status: code,
                reason: format!("log stream upgrade rejected ({status})"),
            },
        };
    }
    ApiError::Other(anyhow::anyhow!("failed to upgrade to WebSocket: {e}"))
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use reqwest_websocket::{CloseCode, Message};

    #[test]
    fn text_frame_parses_into_a_log_message() {
        let json = r#"{"log_type":"stdout","timestamp_ms":1,"state":null,"message":"hi"}"#;
        let item = classify_frame(Message::Text(json.to_string())).expect("text yields an item");
        let log = item.expect("valid json parses");
        assert_eq!(log.log_type, "stdout");
        assert_eq!(log.message.as_deref(), Some("hi"));
    }

    #[test]
    fn malformed_text_frame_is_an_error_item() {
        let item = classify_frame(Message::Text("not json".to_string())).expect("yields an item");
        assert!(
            item.is_err(),
            "a parse failure must surface as an error item"
        );
    }

    #[test]
    fn normal_close_ends_the_stream_cleanly() {
        let frame = Message::Close {
            code: CloseCode::Normal,
            reason: String::new(),
        };
        assert!(
            classify_frame(frame).is_none(),
            "a normal close is a clean end, not an item"
        );
    }

    #[test]
    fn abnormal_close_surfaces_as_an_error() {
        let frame = Message::Close {
            code: CloseCode::Error,
            reason: "boom".into(),
        };
        let item = classify_frame(frame).expect("abnormal close yields an item");
        let err = item.unwrap_err();
        assert!(
            format!("{err:#}").contains("boom"),
            "abnormal close must error with the server reason: {err:#}"
        );
    }

    #[test]
    fn control_frames_are_ignored() {
        assert!(classify_frame(Message::Ping(Vec::new().into())).is_none());
        assert!(classify_frame(Message::Pong(Vec::new().into())).is_none());
        assert!(classify_frame(Message::Binary(Vec::new().into())).is_none());
    }
}
