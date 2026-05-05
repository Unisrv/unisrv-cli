//! Translate parsed HCL ([`UpConfig`]) into a normalized [`DesiredState`].
//!
//! `DesiredState` mirrors what we expect to find on the server after a successful
//! apply: the same shape as the API's `HTTPServiceConfig` and `DeploymentConfiguration`,
//! with all defaults filled in. The diff layer compares this against the
//! observed `CurrentState` field-by-field.

use std::collections::BTreeMap;

use unisrv_api::models::{
    DeploymentConfiguration, HTTPLocation, HTTPLocationTarget, HTTPServiceConfig,
};

use super::config::UpConfig;
use super::defaults::*;

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredState {
    pub project: String,
    /// Keyed by service name (HCL block label).
    pub services: BTreeMap<String, DesiredService>,
    /// Keyed by deployment name (HCL block label).
    pub deployments: BTreeMap<String, DesiredDeployment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredService {
    pub name: String,
    pub host: String,
    pub region: String,
    pub configuration: HTTPServiceConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredDeployment {
    pub name: String,
    pub configuration: DeploymentConfiguration,
    /// Service name (HCL label) this deployment binds to. Resolved to a service_id at apply time.
    pub service_binding: Option<DesiredServiceBinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredServiceBinding {
    pub service_name: String,
    pub target_group: String,
}

impl DesiredState {
    pub fn from_config(cfg: UpConfig) -> Self {
        let project = cfg.project;

        let services = cfg
            .service
            .into_iter()
            .map(|(name, block)| {
                let configuration = HTTPServiceConfig {
                    locations: vec![HTTPLocation {
                        path: DEFAULT_LOCATION_PATH.to_string(),
                        override_404: None,
                        target: HTTPLocationTarget::Instance {
                            group: DEFAULT_TARGET_GROUP.to_string(),
                        },
                    }],
                    allow_http: DEFAULT_ALLOW_HTTP,
                };
                let svc = DesiredService {
                    name: name.clone(),
                    host: block.host,
                    region: DEFAULT_REGION.to_string(),
                    configuration,
                };
                (name, svc)
            })
            .collect();

        let deployments = cfg
            .deployment
            .into_iter()
            .map(|(name, block)| {
                let configuration = DeploymentConfiguration {
                    replicas: DEFAULT_REPLICAS,
                    region: DEFAULT_REGION.to_string(),
                    container_image: block.container.image,
                    args: block.container.args,
                    env: block.container.env,
                    vcpu_ratio: DEFAULT_VCPU_RATIO,
                    vcpu_count: DEFAULT_VCPU_COUNT,
                    memory_mb: DEFAULT_MEMORY_MB,
                    network: None,
                    instance_port: block.port,
                };
                let service_binding = block.service.map(|svc| DesiredServiceBinding {
                    service_name: svc,
                    target_group: DEFAULT_TARGET_GROUP.to_string(),
                });
                let dep = DesiredDeployment {
                    name: name.clone(),
                    configuration,
                    service_binding,
                };
                (name, dep)
            })
            .collect();

        DesiredState {
            project,
            services,
            deployments,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> DesiredState {
        let cfg = UpConfig::parse(src).unwrap();
        DesiredState::from_config(cfg)
    }

    #[test]
    fn fills_in_service_defaults() {
        let state = parse(
            r#"
project = "demo"
service "web" { host = "web.example.com" }
"#,
        );
        let svc = &state.services["web"];
        assert_eq!(svc.name, "web");
        assert_eq!(svc.host, "web.example.com");
        assert_eq!(svc.region, DEFAULT_REGION);
        assert_eq!(svc.configuration.allow_http, false);
        assert_eq!(svc.configuration.locations.len(), 1);
        let loc = &svc.configuration.locations[0];
        assert_eq!(loc.path, "/");
        match &loc.target {
            HTTPLocationTarget::Instance { group } => assert_eq!(group, DEFAULT_TARGET_GROUP),
            _ => panic!("unexpected target"),
        }
    }

    #[test]
    fn fills_in_deployment_defaults() {
        let state = parse(
            r#"
project = "demo"
service "web" { host = "web.example.com" }
deployment "web" {
  service = "web"
  port    = 8080
  container { image = "myapp:1" }
}
"#,
        );
        let dep = &state.deployments["web"];
        assert_eq!(dep.configuration.replicas, DEFAULT_REPLICAS);
        assert_eq!(dep.configuration.region, DEFAULT_REGION);
        assert_eq!(dep.configuration.container_image, "myapp:1");
        assert_eq!(dep.configuration.vcpu_count, DEFAULT_VCPU_COUNT);
        assert_eq!(dep.configuration.vcpu_ratio, DEFAULT_VCPU_RATIO);
        assert_eq!(dep.configuration.memory_mb, DEFAULT_MEMORY_MB);
        assert_eq!(dep.configuration.instance_port, Some(8080));
        assert!(dep.configuration.network.is_none());
        assert!(dep.configuration.args.is_none());

        let binding = dep.service_binding.as_ref().unwrap();
        assert_eq!(binding.service_name, "web");
        assert_eq!(binding.target_group, DEFAULT_TARGET_GROUP);
    }

    #[test]
    fn deployment_without_service_has_no_binding() {
        let state = parse(
            r#"
project = "demo"
deployment "worker" {
  container { image = "w:1" }
}
"#,
        );
        let dep = &state.deployments["worker"];
        assert!(dep.service_binding.is_none());
        assert!(dep.configuration.instance_port.is_none());
    }

    #[test]
    fn passes_through_args_and_env() {
        let state = parse(
            r#"
project = "demo"
deployment "x" {
  container {
    image = "i"
    args  = ["a", "b"]
    env = { K = "v" }
  }
}
"#,
        );
        let dep = &state.deployments["x"];
        assert_eq!(
            dep.configuration.args.as_ref().unwrap(),
            &vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(dep.configuration.env.as_ref().unwrap()["K"], "v");
    }
}
