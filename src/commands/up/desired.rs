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

use crate::commands::host::normalize_host;

use super::config::UpConfig;
use super::defaults::*;

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredState {
    pub project: String,
    /// Keyed by service name (HCL block label).
    pub services: BTreeMap<String, DesiredService>,
    /// Keyed by deployment name (HCL block label).
    pub deployments: BTreeMap<String, DesiredDeployment>,
    /// Keyed by network name (HCL block label).
    pub networks: BTreeMap<String, DesiredNetwork>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredNetwork {
    pub name: String,
    pub ipv4_cidr: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredService {
    pub name: String,
    /// Custom hosts to bind to this service. May be empty — the service is
    /// always reachable at its derived base host regardless.
    pub hosts: Vec<String>,
    pub region: String,
    pub configuration: HTTPServiceConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredDeployment {
    pub name: String,
    pub configuration: DeploymentConfiguration,
    /// Service name (HCL label) this deployment binds to. Resolved to a service_id at apply time.
    pub service_binding: Option<DesiredServiceBinding>,
    /// Network name (HCL label) whose network all instances join. Resolved to
    /// a network_id at apply time — the diff compares names only.
    pub network: Option<String>,
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
                    // Canonicalize (lowercase, strip trailing dot) so the claim,
                    // the link/unlink set-diff, and reachability all agree.
                    hosts: block
                        .hosts
                        .unwrap_or_default()
                        .iter()
                        .map(|h| normalize_host(h))
                        .collect(),
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
                    network: block.network,
                };
                (name, dep)
            })
            .collect();

        let networks = cfg
            .network
            .into_iter()
            .map(|(name, block)| {
                let net = DesiredNetwork {
                    name: name.clone(),
                    ipv4_cidr: block
                        .iprange
                        .unwrap_or_else(|| DEFAULT_NETWORK_CIDR.to_string()),
                };
                (name, net)
            })
            .collect();

        DesiredState {
            project,
            services,
            deployments,
            networks,
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
    fn canonicalizes_hosts_lowercase_and_strips_trailing_dot() {
        // DNS is case-insensitive and FQDNs may carry a trailing dot. Canonicalize
        // at parse so the claim, the link/unlink diff, and the reachability output
        // all use one spelling — no churn, and no uppercase-base-host 400 at claim.
        let state = parse(
            r#"
project = "demo"
service "web" { hosts = ["Web.Example.COM.", "myapp.unisrv.dev"] }
"#,
        );
        assert_eq!(
            state.services["web"].hosts,
            vec![
                "web.example.com".to_string(),
                "myapp.unisrv.dev".to_string()
            ]
        );
    }

    #[test]
    fn fills_in_service_defaults() {
        let state = parse(
            r#"
project = "demo"
service "web" { hosts = ["web.example.com"] }
"#,
        );
        let svc = &state.services["web"];
        assert_eq!(svc.name, "web");
        assert_eq!(svc.hosts, vec!["web.example.com".to_string()]);
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
service "web" { hosts = ["web.example.com"] }
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
        assert!(dep.configuration.args.is_none());

        let binding = dep.service_binding.as_ref().unwrap();
        assert_eq!(binding.service_name, "web");
        assert_eq!(binding.target_group, DEFAULT_TARGET_GROUP);
    }

    #[test]
    fn network_block_fills_default_cidr_and_deployment_carries_network_name() {
        let state = parse(
            r#"
project = "demo"
network "internal" {}
network "backend" { iprange = "10.2.0.0/24" }
deployment "api" {
  network = "internal"
  container { image = "i:1" }
}
"#,
        );
        assert_eq!(state.networks["internal"].name, "internal");
        assert_eq!(state.networks["internal"].ipv4_cidr, DEFAULT_NETWORK_CIDR);
        assert_eq!(state.networks["backend"].ipv4_cidr, "10.2.0.0/24");
        assert_eq!(
            state.deployments["api"].network.as_deref(),
            Some("internal")
        );
    }

    #[test]
    fn deployment_without_network_has_none() {
        let state = parse(
            r#"
project = "demo"
deployment "worker" {
  container { image = "w:1" }
}
"#,
        );
        assert!(state.deployments["worker"].network.is_none());
        assert!(state.networks.is_empty());
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
