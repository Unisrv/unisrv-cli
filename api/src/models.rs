use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

// ── Environments ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateEnvironmentRequest {
    pub project: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateEnvironmentRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentResponse {
    pub id: Uuid,
    pub project: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentListEntry {
    pub id: Uuid,
    pub project: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub instance_count: i64,
    pub service_count: i64,
    pub deployment_count: i64,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentListResponse {
    pub environments: Vec<EnvironmentListEntry>,
}

// ── Instances ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceConfiguration {
    pub container_image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceNetworkConfig {
    pub network_id: Uuid,
    pub instance_ip: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceProvisionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub region: String,
    pub vcpu_ratio: f64,
    pub vcpu_count: u8,
    pub memory_mb: u32,
    pub configuration: InstanceConfiguration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_registry_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<InstanceNetworkConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceProvisionResponse {
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceDeprovisionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceState(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceListEntry {
    pub id: Uuid,
    pub name: Option<String>,
    pub state: InstanceState,
    pub container_image: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceListResponse {
    pub instances: Vec<InstanceListEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceTargetInfo {
    pub id: Uuid,
    pub service_id: Uuid,
    pub service_name: String,
    pub instance_port: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxiedPortInfo {
    pub id: Uuid,
    pub port: u16,
    pub external_address: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceDetailResponse {
    pub id: Uuid,
    pub name: Option<String>,
    pub node_id: Uuid,
    pub state: InstanceState,
    pub exit_code: Option<i32>,
    pub exit_reason: Option<String>,
    pub configuration: serde_json::Value,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub network_id: Option<Uuid>,
    pub network_ip: Option<String>,
    pub service_targets: Option<Vec<ServiceTargetInfo>>,
    pub proxied_ports: Option<Vec<ProxiedPortInfo>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogMessage {
    pub log_type: String,
    pub timestamp_ms: u64,
    pub state: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateInstanceTCPProxyRequest {
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateInstanceTCPProxyResponse {
    pub id: Uuid,
    pub external_address: String,
}

// ── Networks ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateInternalNetworkRequest {
    pub name: String,
    pub ipv4_cidr: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkListItem {
    pub id: Uuid,
    pub name: String,
    pub ipv4_cidr: String,
    pub instance_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkListResponse {
    pub networks: Vec<NetworkListItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub id: Uuid,
    pub internal_ip: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkResponse {
    pub id: Uuid,
    pub name: String,
    pub ipv4_cidr: String,
    pub created_at: NaiveDateTime,
    pub instances: Vec<InstanceInfo>,
}

// ── Services ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HTTPLocationTarget {
    Instance { group: String },
    Url { url: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HTTPLocation {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub override_404: Option<String>,
    pub target: HTTPLocationTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HTTPServiceConfig {
    pub locations: Vec<HTTPLocation>,
    pub allow_http: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceInstanceTarget {
    pub instance_id: Uuid,
    pub instance_port: u16,
    pub group: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceProvisionRequest {
    pub region: String,
    pub name: String,
    pub host: String,
    pub configuration: HTTPServiceConfig,
    pub instance_targets: Vec<ServiceInstanceTarget>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceProvisionResponse {
    pub service_id: Uuid,
    pub connection_string: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceListItem {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceListResponse {
    pub services: Vec<ServiceListItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceProviderDetail {
    pub id: Uuid,
    pub node_id: Uuid,
    pub route_address: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceTargetDetail {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub target_group: String,
    pub instance_port: u16,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceStatistics {
    pub incoming_bytes: u64,
    pub outgoing_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceDetailResponse {
    pub id: Uuid,
    pub name: String,
    pub configuration: serde_json::Value,
    pub environment_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub providers: Vec<ServiceProviderDetail>,
    pub targets: Vec<ServiceTargetDetail>,
    pub statistics: Option<ServiceStatistics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateTargetResponse {
    pub target_id: Uuid,
}

// ── Service Hosts ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimHostRequest {
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostResponse {
    pub id: Uuid,
    pub host: String,
    pub user_id: Uuid,
    pub service_id: Option<Uuid>,
    pub certificate_type: Option<String>,
    pub certificate_valid_until: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DnsConfigResponse {
    pub ipv4_addresses: Vec<Ipv4Addr>,
    pub ipv6_addresses: Vec<Ipv6Addr>,
}

// ── Deployments ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentConfiguration {
    pub replicas: u32,
    pub region: String,
    pub container_image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    pub vcpu_ratio: f64,
    pub vcpu_count: u8,
    pub memory_mb: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentServiceBinding {
    pub service_id: Uuid,
    pub target_group: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateDeploymentRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<DeploymentServiceBinding>,
    pub configuration: DeploymentConfiguration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateDeploymentRequest {
    pub configuration: DeploymentConfiguration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateDeploymentResponse {
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentState(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentListEntry {
    pub id: Uuid,
    pub name: String,
    pub state: DeploymentState,
    pub replicas: u32,
    pub container_image: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentListResponse {
    pub deployments: Vec<DeploymentListEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentInstanceEntry {
    pub id: Uuid,
    pub name: Option<String>,
    pub state: InstanceState,
    pub node_id: Uuid,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentDetailResponse {
    pub id: Uuid,
    pub name: String,
    pub state: DeploymentState,
    pub configuration: DeploymentConfiguration,
    pub metadata: serde_json::Value,
    pub service_id: Option<Uuid>,
    pub service_target_group: Option<String>,
    pub instances: Vec<DeploymentInstanceEntry>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

// ── Container Registries ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryKind {
    Userpass,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserpassConfig {
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserpassSecret {
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateRegistryRequest {
    pub hostname: String,
    pub kind: RegistryKind,
    pub config: serde_json::Value,
    pub secret: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateRegistryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryResponse {
    pub id: Uuid,
    pub hostname: String,
    pub kind: RegistryKind,
    pub config: serde_json::Value,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryListResponse {
    pub registries: Vec<RegistryResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRegistryResponse {
    pub ok: bool,
    #[serde(default)]
    pub expires_in_seconds: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}
