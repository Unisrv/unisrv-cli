//! Test doubles for [`ApiClient`], available behind the `test-support` feature.

use async_trait::async_trait;
use chrono::Duration;
use futures_util::StreamExt;
use std::collections::VecDeque;
use std::sync::Mutex;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::client::{ApiClient, LogStream};
use crate::error::{ApiError, Result};
use crate::models::*;

/// Scripted outcome for a [`MockApiClient::stream_instance_logs`] call.
pub enum StreamLogsResponse {
    /// The upgrade failed before any frame arrived (e.g. instance not found).
    ConnectError(ApiError),
    /// The stream connected and yields these frames in order, then closes —
    /// modelling history replay followed by the server closing the connection.
    Frames(Vec<Result<LogMessage>>),
}

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
    pub link_host_calls: Vec<(Uuid, Uuid)>,
    pub unlink_host_calls: Vec<(Uuid, Uuid)>,
    pub list_hosts_calls: u32,
    pub list_environments_calls: u32,
    pub create_environment_calls: Vec<CreateEnvironmentRequest>,
    pub delete_environment_calls: Vec<Uuid>,
    pub list_instances_calls: Vec<Uuid>,
    pub get_instance_logs_calls: Vec<(Uuid, Uuid)>,
    pub stream_instance_logs_calls: Vec<(Uuid, Uuid)>,
    pub deprovision_instance_calls: Vec<(Uuid, Uuid, Option<InstanceDeprovisionRequest>)>,
    pub create_network_calls: Vec<(Uuid, CreateInternalNetworkRequest)>,
    pub delete_network_calls: Vec<(Uuid, Uuid)>,
    pub list_networks_calls: Vec<Uuid>,
    pub get_network_calls: Vec<(Uuid, Uuid)>,
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
    pub create_registry_calls: Vec<(CreateRegistryRequest, bool)>,
    pub list_registries_calls: u32,
    pub update_registry_calls: Vec<(Uuid, UpdateRegistryRequest, bool)>,
    pub delete_registry_calls: Vec<Uuid>,
    pub test_registry_calls: Vec<Uuid>,
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
    pub link_host_responses: Mutex<VecDeque<std::result::Result<HostResponse, ApiError>>>,
    pub unlink_host_responses: Mutex<VecDeque<std::result::Result<HostResponse, ApiError>>>,
    pub list_hosts_response: ResponseSlot<Vec<HostResponse>>,
    pub list_environments_response: ResponseSlot<EnvironmentListResponse>,
    pub create_environment_response: ResponseSlot<EnvironmentResponse>,
    pub delete_environment_responses: Mutex<VecDeque<std::result::Result<(), ApiError>>>,
    pub list_instances_responses:
        Mutex<VecDeque<std::result::Result<InstanceListResponse, ApiError>>>,
    pub get_instance_logs_responses:
        Mutex<VecDeque<std::result::Result<Vec<LogMessage>, ApiError>>>,
    pub stream_logs_responses: Mutex<VecDeque<StreamLogsResponse>>,
    pub deprovision_instance_responses: Mutex<VecDeque<std::result::Result<(), ApiError>>>,
    pub create_network_responses: Mutex<VecDeque<std::result::Result<NetworkResponse, ApiError>>>,
    pub delete_network_responses: Mutex<VecDeque<std::result::Result<(), ApiError>>>,
    pub list_networks_response: ResponseSlot<NetworkListResponse>,
    /// Queue popped FIFO by each `get_network` call — a queue (not a one-shot
    /// slot) because the network drain poll gets the same network repeatedly.
    pub get_network_responses: Mutex<VecDeque<std::result::Result<NetworkResponse, ApiError>>>,
    pub list_services_response: ResponseSlot<ServiceListResponse>,
    pub get_service_responses:
        Mutex<VecDeque<std::result::Result<ServiceDetailResponse, ApiError>>>,
    /// Queue of responses popped FIFO by each `list_deployments` call. A queue
    /// (not a one-shot slot) because `destroy`'s drain poll lists repeatedly.
    pub list_deployments_responses:
        Mutex<VecDeque<std::result::Result<DeploymentListResponse, ApiError>>>,
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
    pub create_registry_responses: Mutex<VecDeque<std::result::Result<RegistryResponse, ApiError>>>,
    pub list_registries_response: ResponseSlot<RegistryListResponse>,
    pub update_registry_responses: Mutex<VecDeque<std::result::Result<RegistryResponse, ApiError>>>,
    pub delete_registry_responses: Mutex<VecDeque<std::result::Result<(), ApiError>>>,
    pub test_registry_responses:
        Mutex<VecDeque<std::result::Result<TestRegistryResponse, ApiError>>>,
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
            link_host_responses: Mutex::new(VecDeque::new()),
            unlink_host_responses: Mutex::new(VecDeque::new()),
            list_hosts_response: ResponseSlot::default(),
            list_environments_response: ResponseSlot::default(),
            create_environment_response: ResponseSlot::default(),
            delete_environment_responses: Mutex::new(VecDeque::new()),
            list_instances_responses: Mutex::new(VecDeque::new()),
            get_instance_logs_responses: Mutex::new(VecDeque::new()),
            stream_logs_responses: Mutex::new(VecDeque::new()),
            deprovision_instance_responses: Mutex::new(VecDeque::new()),
            create_network_responses: Mutex::new(VecDeque::new()),
            delete_network_responses: Mutex::new(VecDeque::new()),
            list_networks_response: ResponseSlot::default(),
            get_network_responses: Mutex::new(VecDeque::new()),
            list_services_response: ResponseSlot::default(),
            get_service_responses: Mutex::new(VecDeque::new()),
            list_deployments_responses: Mutex::new(VecDeque::new()),
            get_deployment_responses: Mutex::new(VecDeque::new()),
            provision_service_responses: Mutex::new(VecDeque::new()),
            create_deployment_responses: Mutex::new(VecDeque::new()),
            update_service_responses: Mutex::new(VecDeque::new()),
            update_deployment_responses: Mutex::new(VecDeque::new()),
            delete_service_responses: Mutex::new(VecDeque::new()),
            delete_deployment_responses: Mutex::new(VecDeque::new()),
            create_registry_responses: Mutex::new(VecDeque::new()),
            list_registries_response: ResponseSlot::default(),
            update_registry_responses: Mutex::new(VecDeque::new()),
            delete_registry_responses: Mutex::new(VecDeque::new()),
            test_registry_responses: Mutex::new(VecDeque::new()),
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

    pub fn push_create_network(self, resp: std::result::Result<NetworkResponse, ApiError>) -> Self {
        self.create_network_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_delete_network(self, resp: std::result::Result<(), ApiError>) -> Self {
        self.delete_network_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn with_list_networks(
        self,
        resp: std::result::Result<NetworkListResponse, ApiError>,
    ) -> Self {
        self.list_networks_response.set(resp);
        self
    }

    /// Queue one `get_network` response. Each call pops the next, so chain
    /// multiple to script a drain sequence (instances present, then empty).
    pub fn push_get_network(self, resp: std::result::Result<NetworkResponse, ApiError>) -> Self {
        self.get_network_responses.lock().unwrap().push_back(resp);
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

    /// Queue one `list_deployments` response. Each call pops the next, so chain
    /// multiple to script a drain sequence (e.g. non-empty, non-empty, empty).
    pub fn with_list_deployments(
        self,
        resp: std::result::Result<DeploymentListResponse, ApiError>,
    ) -> Self {
        self.list_deployments_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_delete_environment(self, resp: std::result::Result<(), ApiError>) -> Self {
        self.delete_environment_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn with_list_instances(
        self,
        resp: std::result::Result<InstanceListResponse, ApiError>,
    ) -> Self {
        self.list_instances_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    /// Queue one `get_instance_logs` response.
    pub fn push_instance_logs(self, resp: std::result::Result<Vec<LogMessage>, ApiError>) -> Self {
        self.get_instance_logs_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    /// Queue a log stream that yields these frames (each as a success) and then
    /// closes — the common "history replays, then the instance stops" case.
    pub fn push_stream_logs(self, frames: Vec<LogMessage>) -> Self {
        self.stream_logs_responses
            .lock()
            .unwrap()
            .push_back(StreamLogsResponse::Frames(
                frames.into_iter().map(Ok).collect(),
            ));
        self
    }

    /// Queue a log stream with explicit per-frame results, so a test can inject
    /// a mid-stream transport error after some good frames.
    pub fn push_stream_logs_frames(self, frames: Vec<Result<LogMessage>>) -> Self {
        self.stream_logs_responses
            .lock()
            .unwrap()
            .push_back(StreamLogsResponse::Frames(frames));
        self
    }

    /// Queue a log stream whose connection (upgrade) fails before any frame.
    pub fn push_stream_connect_error(self, err: ApiError) -> Self {
        self.stream_logs_responses
            .lock()
            .unwrap()
            .push_back(StreamLogsResponse::ConnectError(err));
        self
    }

    pub fn push_deprovision_instance(self, resp: std::result::Result<(), ApiError>) -> Self {
        self.deprovision_instance_responses
            .lock()
            .unwrap()
            .push_back(resp);
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

    pub fn push_link_host(self, resp: std::result::Result<HostResponse, ApiError>) -> Self {
        self.link_host_responses.lock().unwrap().push_back(resp);
        self
    }

    pub fn push_unlink_host(self, resp: std::result::Result<HostResponse, ApiError>) -> Self {
        self.unlink_host_responses.lock().unwrap().push_back(resp);
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

    pub fn push_create_registry(
        self,
        resp: std::result::Result<RegistryResponse, ApiError>,
    ) -> Self {
        self.create_registry_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn with_list_registries(
        self,
        resp: std::result::Result<RegistryListResponse, ApiError>,
    ) -> Self {
        self.list_registries_response.set(resp);
        self
    }

    pub fn push_update_registry(
        self,
        resp: std::result::Result<RegistryResponse, ApiError>,
    ) -> Self {
        self.update_registry_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_delete_registry(self, resp: std::result::Result<(), ApiError>) -> Self {
        self.delete_registry_responses
            .lock()
            .unwrap()
            .push_back(resp);
        self
    }

    pub fn push_test_registry(
        self,
        resp: std::result::Result<TestRegistryResponse, ApiError>,
    ) -> Self {
        self.test_registry_responses.lock().unwrap().push_back(resp);
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
    async fn delete_environment(&self, id: Uuid) -> Result<()> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("delete_environment");
            calls.delete_environment_calls.push(id);
        }
        self.delete_environment_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("delete_environment_response not configured"))
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
        env_id: Uuid,
        instance_id: Uuid,
        req: Option<InstanceDeprovisionRequest>,
    ) -> Result<()> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("deprovision_instance");
            calls
                .deprovision_instance_calls
                .push((env_id, instance_id, req));
        }
        self.deprovision_instance_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("deprovision_instance_response not configured"))
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
    async fn list_instances(&self, env_id: Uuid) -> Result<InstanceListResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("list_instances");
            calls.list_instances_calls.push(env_id);
        }
        self.list_instances_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("list_instances_response not configured"))
    }
    async fn get_instance_logs(&self, env_id: Uuid, instance_id: Uuid) -> Result<Vec<LogMessage>> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("get_instance_logs");
            calls.get_instance_logs_calls.push((env_id, instance_id));
        }
        self.get_instance_logs_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("get_instance_logs_response not configured"))
    }
    async fn stream_instance_logs(&self, env_id: Uuid, instance_id: Uuid) -> Result<LogStream> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("stream_instance_logs");
            calls.stream_instance_logs_calls.push((env_id, instance_id));
        }
        match self
            .stream_logs_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("stream_instance_logs_response not configured"))
        {
            StreamLogsResponse::ConnectError(err) => Err(err),
            StreamLogsResponse::Frames(frames) => Ok(futures_util::stream::iter(frames).boxed()),
        }
    }
    async fn create_tcp_proxy(
        &self,
        _: Uuid,
        _: Uuid,
        _: CreateInstanceTCPProxyRequest,
    ) -> Result<CreateInstanceTCPProxyResponse> {
        unimplemented!()
    }
    async fn create_network(
        &self,
        env_id: Uuid,
        req: CreateInternalNetworkRequest,
    ) -> Result<NetworkResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("create_network");
            calls.create_network_calls.push((env_id, req));
        }
        self.create_network_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("create_network_response not configured"))
    }
    async fn delete_network(&self, env_id: Uuid, network_id: Uuid) -> Result<()> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("delete_network");
            calls.delete_network_calls.push((env_id, network_id));
        }
        self.delete_network_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("delete_network_response not configured"))
    }
    async fn list_networks(&self, env_id: Uuid, _: bool) -> Result<NetworkListResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("list_networks");
            calls.list_networks_calls.push(env_id);
        }
        self.list_networks_response.take("list_networks_response")
    }
    async fn get_network(&self, env_id: Uuid, network_id: Uuid) -> Result<NetworkResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("get_network");
            calls.get_network_calls.push((env_id, network_id));
        }
        self.get_network_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("get_network_response not configured"))
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
            .unwrap_or_else(|| panic!("update_service_response not configured"))
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
            .unwrap_or_else(|| panic!("delete_service_response not configured"))
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
    async fn link_host_to_service(&self, id: Uuid, service_id: Uuid) -> Result<HostResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("link_host_to_service");
            calls.link_host_calls.push((id, service_id));
        }
        self.link_host_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("link_host_response not configured"))
    }
    async fn unlink_host_from_service(&self, id: Uuid, service_id: Uuid) -> Result<HostResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("unlink_host_from_service");
            calls.unlink_host_calls.push((id, service_id));
        }
        self.unlink_host_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unlink_host_response not configured"))
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
        self.list_deployments_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("list_deployments_response not configured"))
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
            .unwrap_or_else(|| panic!("update_deployment_response not configured"))
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
            .unwrap_or_else(|| panic!("delete_deployment_response not configured"))
    }

    async fn create_registry(
        &self,
        req: CreateRegistryRequest,
        validate: bool,
    ) -> Result<RegistryResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("create_registry");
            calls.create_registry_calls.push((req, validate));
        }
        self.create_registry_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("create_registry_response not configured"))
    }

    async fn list_registries(&self) -> Result<RegistryListResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("list_registries");
            calls.list_registries_calls += 1;
        }
        self.list_registries_response
            .take("list_registries_response")
    }

    async fn update_registry(
        &self,
        id: Uuid,
        req: UpdateRegistryRequest,
        validate: bool,
    ) -> Result<RegistryResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("update_registry");
            calls.update_registry_calls.push((id, req, validate));
        }
        self.update_registry_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("update_registry_response not configured"))
    }

    async fn delete_registry(&self, id: Uuid) -> Result<()> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("delete_registry");
            calls.delete_registry_calls.push(id);
        }
        self.delete_registry_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("delete_registry_response not configured"))
    }

    async fn test_registry(&self, id: Uuid) -> Result<TestRegistryResponse> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.call_order.push("test_registry");
            calls.test_registry_calls.push(id);
        }
        self.test_registry_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("test_registry_response not configured"))
    }
}
