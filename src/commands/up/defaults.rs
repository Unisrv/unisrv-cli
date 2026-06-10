//! Default values applied to deployments and services when not specified in HCL.
//!
//! These are intentionally placed at the top of the module — easy to find and tweak
//! while the platform's "default region" / sizing story is still in flux.

pub const DEFAULT_REGION: &str = "dev";
pub const DEFAULT_REPLICAS: u32 = 1;
pub const DEFAULT_VCPU_RATIO: f64 = 0.25;
pub const DEFAULT_VCPU_COUNT: u8 = 1;
pub const DEFAULT_MEMORY_MB: u32 = 256;

pub const DEFAULT_NETWORK_CIDR: &str = "10.0.0.0/16";

pub const DEFAULT_TARGET_GROUP: &str = "default";
pub const DEFAULT_LOCATION_PATH: &str = "/";
pub const DEFAULT_ALLOW_HTTP: bool = false;

pub const DEFAULT_ENV_NAME: &str = "dev";
pub fn default_env_display_name(project: &str) -> String {
    format!("{project} Development")
}
