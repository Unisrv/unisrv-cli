//! Test doubles for [`ApiClient`], available behind the `test-support` feature.

use async_trait::async_trait;
use chrono::Duration;
use std::sync::Mutex;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::client::ApiClient;
use crate::error::{ApiError, Result};
use crate::models::*;

/// Records which methods were called and with what arguments.
#[derive(Default)]
pub struct CallLog {
    pub login_calls: Vec<(String, String)>,
    pub access_token_calls: u32,
    pub auth_session_calls: u32,
    pub claim_host_calls: Vec<ClaimHostRequest>,
    pub get_hosts_dns_config_calls: u32,
    pub request_host_cert_calls: Vec<Uuid>,
    pub list_hosts_calls: u32,
}

/// One-shot response slot for a mocked endpoint. Configure with `set`, consume with `take`.
pub struct ResponseSlot<T>(Mutex<Option<std::result::Result<T, ApiError>>>);

impl<T> Default for ResponseSlot<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<T> ResponseSlot<T> {
    pub fn set(&self, resp: std::result::Result<T, ApiError>) {
        *self.0.lock().unwrap() = Some(resp);
    }

    fn take(&self, name: &'static str) -> Result<T> {
        self.0
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| panic!("{name} not configured on MockApiClient"))
    }
}

pub struct MockApiClient {
    pub login_result: Mutex<Option<std::result::Result<(), ApiError>>>,
    pub session: Mutex<Option<AuthSession>>,
    pub claim_host_response: ResponseSlot<HostResponse>,
    pub dns_config_response: ResponseSlot<DnsConfigResponse>,
    pub request_host_cert_response: ResponseSlot<HostResponse>,
    pub list_hosts_response: ResponseSlot<Vec<HostResponse>>,
    pub calls: Mutex<CallLog>,
}

impl Default for MockApiClient {
    fn default() -> Self {
        MockApiClient {
            login_result: Mutex::new(Some(Ok(()))),
            session: Mutex::new(None),
            claim_host_response: ResponseSlot::default(),
            dns_config_response: ResponseSlot::default(),
            request_host_cert_response: ResponseSlot::default(),
            list_hosts_response: ResponseSlot::default(),
            calls: Mutex::new(CallLog::default()),
        }
    }
}

impl MockApiClient {
    /// Create a mock that is "logged in" with a valid session.
    pub fn logged_in() -> Self {
        let session = AuthSession::test_session("test-token", Duration::hours(1));
        Self {
            session: Mutex::new(Some(session)),
            ..Self::default()
        }
    }

    /// Create a mock with no session (not logged in).
    pub fn logged_out() -> Self {
        Self::default()
    }

    /// Create a mock where login will fail with the given error.
    pub fn login_fails(error: ApiError) -> Self {
        Self {
            login_result: Mutex::new(Some(Err(error))),
            ..Self::default()
        }
    }

    /// Configure the response that the next `claim_host` call will return.
    pub fn with_claim_host(self, resp: std::result::Result<HostResponse, ApiError>) -> Self {
        self.claim_host_response.set(resp);
        self
    }

    /// Configure the response that the next `get_hosts_dns_config` call will return.
    pub fn with_dns_config(self, resp: std::result::Result<DnsConfigResponse, ApiError>) -> Self {
        self.dns_config_response.set(resp);
        self
    }

    /// Configure the response that the next `request_host_cert` call will return.
    pub fn with_request_host_cert(
        self,
        resp: std::result::Result<HostResponse, ApiError>,
    ) -> Self {
        self.request_host_cert_response.set(resp);
        self
    }

    /// Configure the response that the next `list_hosts` call will return.
    pub fn with_list_hosts(self, resp: std::result::Result<Vec<HostResponse>, ApiError>) -> Self {
        self.list_hosts_response.set(resp);
        self
    }

    fn require_session(&self) -> Result<AuthSession> {
        self.session
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(ApiError::not_logged_in)
    }
}

#[async_trait]
impl ApiClient for MockApiClient {
    async fn login(&self, username: &str, password: &str) -> Result<()> {
        self.calls
            .lock()
            .unwrap()
            .login_calls
            .push((username.to_string(), password.to_string()));
        self.login_result.lock().unwrap().take().unwrap_or(Ok(()))
    }

    async fn access_token(&self) -> Result<String> {
        self.calls.lock().unwrap().access_token_calls += 1;
        Ok(self.require_session()?.access_token().to_string())
    }

    async fn auth_session(&self) -> Result<AuthSession> {
        self.calls.lock().unwrap().auth_session_calls += 1;
        self.require_session()
    }

    async fn create_environment(&self, _: CreateEnvironmentRequest) -> Result<EnvironmentResponse> {
        unimplemented!()
    }
    async fn list_environments(&self) -> Result<EnvironmentListResponse> {
        unimplemented!()
    }
    async fn update_environment(
        &self,
        _: Uuid,
        _: UpdateEnvironmentRequest,
    ) -> Result<EnvironmentResponse> {
        unimplemented!()
    }
    async fn delete_environment(&self, _: Uuid) -> Result<()> {
        unimplemented!()
    }
    async fn provision_instance(
        &self,
        _: Uuid,
        _: InstanceProvisionRequest,
    ) -> Result<InstanceProvisionResponse> {
        unimplemented!()
    }
    async fn deprovision_instance(
        &self,
        _: Uuid,
        _: Uuid,
        _: Option<InstanceDeprovisionRequest>,
    ) -> Result<()> {
        unimplemented!()
    }
    async fn get_instance(
        &self,
        _: Uuid,
        _: Uuid,
        _: bool,
        _: bool,
    ) -> Result<InstanceDetailResponse> {
        unimplemented!()
    }
    async fn list_instances(&self, _: Uuid) -> Result<InstanceListResponse> {
        unimplemented!()
    }
    async fn get_instance_logs(&self, _: Uuid, _: Uuid) -> Result<Vec<LogMessage>> {
        unimplemented!()
    }
    async fn create_tcp_proxy(
        &self,
        _: Uuid,
        _: Uuid,
        _: CreateInstanceTCPProxyRequest,
    ) -> Result<CreateInstanceTCPProxyResponse> {
        unimplemented!()
    }
    async fn create_network(&self, _: CreateInternalNetworkRequest) -> Result<NetworkResponse> {
        unimplemented!()
    }
    async fn delete_network(&self, _: Uuid) -> Result<()> {
        unimplemented!()
    }
    async fn list_networks(&self, _: bool) -> Result<NetworkListResponse> {
        unimplemented!()
    }
    async fn get_network(&self, _: Uuid) -> Result<NetworkResponse> {
        unimplemented!()
    }
    async fn provision_service(
        &self,
        _: Uuid,
        _: ServiceProvisionRequest,
    ) -> Result<ServiceProvisionResponse> {
        unimplemented!()
    }
    async fn list_services(&self, _: Uuid) -> Result<ServiceListResponse> {
        unimplemented!()
    }
    async fn get_service(&self, _: Uuid, _: Uuid) -> Result<ServiceDetailResponse> {
        unimplemented!()
    }
    async fn update_service(&self, _: Uuid, _: Uuid, _: HTTPServiceConfig) -> Result<()> {
        unimplemented!()
    }
    async fn delete_service(&self, _: Uuid, _: Uuid) -> Result<()> {
        unimplemented!()
    }
    async fn create_service_target(
        &self,
        _: Uuid,
        _: Uuid,
        _: ServiceInstanceTarget,
    ) -> Result<CreateTargetResponse> {
        unimplemented!()
    }
    async fn delete_service_target(&self, _: Uuid, _: Uuid, _: Uuid) -> Result<()> {
        unimplemented!()
    }
    async fn claim_host(&self, req: ClaimHostRequest) -> Result<HostResponse> {
        self.calls.lock().unwrap().claim_host_calls.push(req);
        self.claim_host_response.take("claim_host_response")
    }
    async fn list_hosts(&self) -> Result<Vec<HostResponse>> {
        self.calls.lock().unwrap().list_hosts_calls += 1;
        self.list_hosts_response.take("list_hosts_response")
    }
    async fn delete_host(&self, _: Uuid) -> Result<()> {
        unimplemented!()
    }
    async fn request_host_cert(&self, id: Uuid) -> Result<HostResponse> {
        self.calls.lock().unwrap().request_host_cert_calls.push(id);
        self.request_host_cert_response
            .take("request_host_cert_response")
    }
    async fn get_hosts_dns_config(&self) -> Result<DnsConfigResponse> {
        self.calls.lock().unwrap().get_hosts_dns_config_calls += 1;
        self.dns_config_response.take("dns_config_response")
    }
    async fn create_deployment(
        &self,
        _: Uuid,
        _: CreateDeploymentRequest,
    ) -> Result<CreateDeploymentResponse> {
        unimplemented!()
    }
    async fn list_deployments(&self, _: Uuid) -> Result<DeploymentListResponse> {
        unimplemented!()
    }
    async fn get_deployment(&self, _: Uuid, _: Uuid) -> Result<DeploymentDetailResponse> {
        unimplemented!()
    }
    async fn update_deployment(&self, _: Uuid, _: Uuid, _: UpdateDeploymentRequest) -> Result<()> {
        unimplemented!()
    }
    async fn delete_deployment(&self, _: Uuid, _: Uuid) -> Result<()> {
        unimplemented!()
    }
}
