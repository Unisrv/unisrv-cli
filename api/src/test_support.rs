//! Test doubles for [`ApiClient`], available behind the `test-support` feature.

use async_trait::async_trait;
use chrono::Duration;
use std::collections::VecDeque;
use std::sync::Mutex;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::client::ApiClient;
use crate::error::{ApiError, Result};
use crate::models::*;

/// Records which methods were called and with what arguments.
#[derive(Default)]
pub struct CallLog {
    /// Method names in the order they were invoked. Use this to assert
    /// cross-method ordering (e.g. that all `delete_deployment` calls
    /// preceded any `delete_service` call).
    pub call_order: Vec<&'static str>,
    pub login_calls: Vec<(String, String)>,
    pub access_token_calls: u32,
    pub auth_session_calls: u32,
    pub claim_host_calls: Vec<ClaimHostRequest>,
    pub get_hosts_dns_config_calls: u32,
    pub request_host_cert_calls: Vec<Uuid>,
    pub list_hosts_calls: u32,
    pub list_environments_calls: u32,
    pub create_environment_calls: Vec<CreateEnvironmentRequest>,
    pub list_services_calls: Vec<Uuid>,
    pub get_service_calls: Vec<(Uuid, Uuid)>,
    pub list_deployments_calls: Vec<Uuid>,
    pub get_deployment_calls: Vec<(Uuid, Uuid)>,
    pub provision_service_calls: Vec<(Uuid, ServiceProvisionRequest)>,
    pub create_deployment_calls: Vec<(Uuid, CreateDeploymentRequest)>,
    pub update_service_calls: Vec<(Uuid, Uuid, HTTPServiceConfig)>,
    pub update_deployment_calls: Vec<(Uuid, Uuid, UpdateDeploymentRequest)>,
    pub delete_service_calls: Vec<(Uuid, Uuid)>,
    pub delete_deployment_calls: Vec<(Uuid, Uuid)>,
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
    pub list_environments_response: ResponseSlot<EnvironmentListResponse>,
    pub create_environment_response: ResponseSlot<EnvironmentResponse>,
    pub list_services_response: ResponseSlot<ServiceListResponse>,
    pub get_service_responses:
        Mutex<VecDeque<std::result::Result<ServiceDetailResponse, ApiError>>>,
    pub list_deployments_response: ResponseSlot<DeploymentListResponse>,
    pub get_deployment_responses:
        Mutex<VecDeque<std::result::Result<DeploymentDetailResponse, ApiError>>>,
    pub provision_service_responses:
        Mutex<VecDeque<std::result::Result<ServiceProvisionResponse, ApiError>>>,
    pub create_deployment_responses:
        Mutex<VecDeque<std::result::Result<CreateDeploymentResponse, ApiError>>>,
    pub update_service_responses: Mutex<VecDeque<std::result::Result<(), ApiError>>>,
    pub update_deployment_responses: Mutex<VecDeque<std::result::Result<(), ApiError>>>,
    pub delete_service_responses: Mutex<VecDeque<std::result::Result<(), ApiError>>>,
    pub delete_deployment_responses: Mutex<VecDeque<std::result::Result<(), ApiError>>>,
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
            list_environments_response: ResponseSlot::default(),
            create_environment_response: ResponseSlot::default(),
            list_services_response: ResponseSlot::default(),
            get_service_responses: Mutex::new(VecDeque::new()),
            list_deployments_response: ResponseSlot::default(),
            get_deployment_responses: Mutex::new(VecDeque::new()),
            provision_service_responses: Mutex::new(VecDeque::new()),
            create_deployment_responses: Mutex::new(VecDeque::new()),
            update_service_responses: Mutex::new(VecDeque::new()),
            update_deployment_responses: Mutex::new(VecDeque::new()),
            delete_service_responses: Mutex::new(VecDeque::new()),
            delete_deployment_responses: Mutex::new(VecDeque::new()),
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
    pub fn with_request_host_cert(self, resp: std::result::Result<HostResponse, ApiError>) -> Self {
        self.request_host_cert_response.set(resp);
        self
    }

    /// Configure the response that the next `list_hosts` call will return.
    pub fn with_list_hosts(self, resp: std::result::Result<Vec<HostResponse>, ApiError>) -> Self {
        self.list_hosts_response.set(resp);
        self
    }

    pub fn with_list_environments(
        self,
        resp: std::result::Result<EnvironmentListResponse, ApiError>,
    ) -> Self {
        self.list_environments_response.set(resp);
        self
    }

    pub fn with_create_environment(
        self,
        resp: std::result::Result<EnvironmentResponse, ApiError>,
    ) -> Self {
        self.create_environment_response.set(resp);
        self
    }

    pub fn with_list_services(
        self,
        resp: std::result::Result<ServiceListResponse, ApiError>,
    ) -> Self {
        self.list_services_response.set(resp);
        self
    }

    pub fn push_get_service(
        self,
        resp: std::result::Result<ServiceDetailResponse, ApiError>,
    ) -> Self {
        self.get_service_responses.lock().unwrap().push_back(resp);
        self
    }

    pub fn with_list_deployments(
        self,
        resp: std::result::Result<DeploymentListResponse, ApiError>,
    ) -> Self {
        self.list_deployments_response.set(resp);
        self
    }

    pub fn push_get_deployment(
        self,
        resp: std::result::Result<DeploymentDetailResponse, ApiError>,
    ) -> Self {
        self.get_deployment_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_provision_service(
        self,
        resp: std::result::Result<ServiceProvisionResponse, ApiError>,
    ) -> Self {
        self.provision_service_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_create_deployment(
        self,
        resp: std::result::Result<CreateDeploymentResponse, ApiError>,
    ) -> Self {
        self.create_deployment_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_update_service(self, resp: std::result::Result<(), ApiError>) -> Self {
        self.update_service_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_update_deployment(self, resp: std::result::Result<(), ApiError>) -> Self {
        self.update_deployment_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_delete_service(self, resp: std::result::Result<(), ApiError>) -> Self {
        self.delete_service_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_delete_deployment(self, resp: std::result::Result<(), ApiError>) -> Self {
        self.delete_deployment_responses
            .lock()
            .unwrap()
            .push_back(resp);
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
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("login");
            calls
                .login_calls
                .push((username.to_string(), password.to_string()));
        }
        self.login_result.lock().unwrap().take().unwrap_or(Ok(()))
    }

    async fn access_token(&self) -> Result<String> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("access_token");
            calls.access_token_calls += 1;
        }
        Ok(self.require_session()?.access_token().to_string())
    }

    async fn auth_session(&self) -> Result<AuthSession> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("auth_session");
            calls.auth_session_calls += 1;
        }
        self.require_session()
    }

    async fn create_environment(
        &self,
        req: CreateEnvironmentRequest,
    ) -> Result<EnvironmentResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("create_environment");
            calls.create_environment_calls.push(req);
        }
        self.create_environment_response
            .take("create_environment_response")
    }
    async fn list_environments(&self) -> Result<EnvironmentListResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("list_environments");
            calls.list_environments_calls += 1;
        }
        self.list_environments_response
            .take("list_environments_response")
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
        env_id: Uuid,
        req: ServiceProvisionRequest,
    ) -> Result<ServiceProvisionResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("provision_service");
            calls.provision_service_calls.push((env_id, req));
        }
        self.provision_service_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("provision_service_response not configured"))
    }
    async fn list_services(&self, env_id: Uuid) -> Result<ServiceListResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("list_services");
            calls.list_services_calls.push(env_id);
        }
        self.list_services_response.take("list_services_response")
    }
    async fn get_service(&self, env_id: Uuid, service_id: Uuid) -> Result<ServiceDetailResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("get_service");
            calls.get_service_calls.push((env_id, service_id));
        }
        self.get_service_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("get_service_response not configured"))
    }
    async fn update_service(
        &self,
        env_id: Uuid,
        service_id: Uuid,
        req: HTTPServiceConfig,
    ) -> Result<()> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("update_service");
            calls.update_service_calls.push((env_id, service_id, req));
        }
        self.update_service_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
    }
    async fn delete_service(&self, env_id: Uuid, service_id: Uuid) -> Result<()> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("delete_service");
            calls.delete_service_calls.push((env_id, service_id));
        }
        self.delete_service_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
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
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("claim_host");
            calls.claim_host_calls.push(req);
        }
        self.claim_host_response.take("claim_host_response")
    }
    async fn list_hosts(&self) -> Result<Vec<HostResponse>> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("list_hosts");
            calls.list_hosts_calls += 1;
        }
        self.list_hosts_response.take("list_hosts_response")
    }
    async fn delete_host(&self, _: Uuid) -> Result<()> {
        unimplemented!()
    }
    async fn request_host_cert(&self, id: Uuid) -> Result<HostResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("request_host_cert");
            calls.request_host_cert_calls.push(id);
        }
        self.request_host_cert_response
            .take("request_host_cert_response")
    }
    async fn get_hosts_dns_config(&self) -> Result<DnsConfigResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("get_hosts_dns_config");
            calls.get_hosts_dns_config_calls += 1;
        }
        self.dns_config_response.take("dns_config_response")
    }
    async fn create_deployment(
        &self,
        env_id: Uuid,
        req: CreateDeploymentRequest,
    ) -> Result<CreateDeploymentResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("create_deployment");
            calls.create_deployment_calls.push((env_id, req));
        }
        self.create_deployment_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("create_deployment_response not configured"))
    }
    async fn list_deployments(&self, env_id: Uuid) -> Result<DeploymentListResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("list_deployments");
            calls.list_deployments_calls.push(env_id);
        }
        self.list_deployments_response
            .take("list_deployments_response")
    }
    async fn get_deployment(
        &self,
        env_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<DeploymentDetailResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("get_deployment");
            calls.get_deployment_calls.push((env_id, deployment_id));
        }
        self.get_deployment_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("get_deployment_response not configured"))
    }
    async fn update_deployment(
        &self,
        env_id: Uuid,
        deployment_id: Uuid,
        req: UpdateDeploymentRequest,
    ) -> Result<()> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("update_deployment");
            calls
                .update_deployment_calls
                .push((env_id, deployment_id, req));
        }
        self.update_deployment_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
    }
    async fn delete_deployment(&self, env_id: Uuid, deployment_id: Uuid) -> Result<()> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("delete_deployment");
            calls.delete_deployment_calls.push((env_id, deployment_id));
        }
        self.delete_deployment_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
    }
}
