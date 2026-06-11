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

use super::config::{LocationTarget, UpConfig};
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

        // A location's deployment reference IS the service binding: the
        // deployment joins the instance group named after it. Collect the
        // bindings service-side before deployments are built.
        let mut bindings: BTreeMap<String, DesiredServiceBinding> = BTreeMap::new();
        for (svc_name, block) in &cfg.service {
            for dep in block.referenced_deployments() {
                bindings.insert(
                    dep.to_string(),
                    DesiredServiceBinding {
                        service_name: svc_name.clone(),
                        target_group: dep.to_string(),
                    },
                );
            }
        }

        let services = cfg
            .service
            .into_iter()
            .map(|(name, block)| {
                let mut locations: Vec<HTTPLocation> = block
                    .resolved_locations()
                    .into_iter()
                    .map(|loc| {
                        // Validation guarantees exactly one target.
                        let target = match loc
                            .target
                            .expect("validation guarantees exactly one location target")
                        {
                            LocationTarget::Url(url) => HTTPLocationTarget::Url { url },
                            LocationTarget::Deployment(group)
                            | LocationTarget::InstanceGroup(group) => {
                                HTTPLocationTarget::Instance { group }
                            }
                        };
                        HTTPLocation {
                            path: loc.path.to_string(),
                            override_404: loc.override_404.map(str::to_string),
                            target,
                        }
                    })
                    .collect();
                if locations.is_empty() {
                    // No routing declared at all: reserve the host with a
                    // catch-all to the (out-of-band) default group.
                    locations.push(HTTPLocation {
                        path: DEFAULT_LOCATION_PATH.to_string(),
                        override_404: None,
                        target: HTTPLocationTarget::Instance {
                            group: DEFAULT_TARGET_GROUP.to_string(),
                        },
                    });
                }
                let configuration = HTTPServiceConfig {
                    locations,
                    allow_http: block.allow_http.unwrap_or(DEFAULT_ALLOW_HTTP),
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
                    replicas: block.replicas.map(|r| r as u32).unwrap_or(DEFAULT_REPLICAS),
                    region: DEFAULT_REGION.to_string(),
                    container_image: block.container.image,
                    args: block.container.args,
                    env: block.container.env,
                    vcpu_ratio: block.vcpu_ratio.unwrap_or(DEFAULT_VCPU_RATIO),
                    vcpu_count: block.vcpus.map(|v| v as u8).unwrap_or(DEFAULT_VCPU_COUNT),
                    memory_mb: block
                        .memory
                        .map(|m| {
                            m.to_mb().expect("validation guarantees a parseable memory") as u32
                        })
                        .unwrap_or(DEFAULT_MEMORY_MB),
                    instance_port: block.port,
                };
                let service_binding = bindings.remove(&name);
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
service "web" {
  hosts = ["web.example.com"]
  location "/" { deployment = "web" }
}
deployment "web" {
  port = 8080
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
        assert_eq!(binding.target_group, "web");
    }

    #[test]
    fn location_deployment_ref_routes_and_binds() {
        // A location's deployment reference does two things: it becomes an
        // instance-group target (group = deployment name) in the service's
        // routing table, and it binds that deployment to the service.
        let state = parse(
            r#"
project = "demo"
service "web" {
  location "/api" { deployment = "api" }
}
deployment "api" {
  port = 8000
  container { image = "api:1" }
}
"#,
        );
        let locations = &state.services["web"].configuration.locations;
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, "/api");
        assert_eq!(
            locations[0].target,
            HTTPLocationTarget::Instance {
                group: "api".into()
            }
        );

        let binding = state.deployments["api"].service_binding.as_ref().unwrap();
        assert_eq!(binding.service_name, "web");
        assert_eq!(binding.target_group, "api");
    }

    #[test]
    fn service_deployment_shorthand_desugars_to_catchall_appended_last() {
        // `deployment = "x"` on a service is sugar for `location "/" { deployment = "x" }`.
        // The proxy matches first-match-wins, so the catch-all must land after
        // every explicit location regardless of where the attribute sits.
        let state = parse(
            r#"
project = "demo"
service "web" {
  deployment = "frontend"
  location "/api" { deployment = "api" }
}
deployment "frontend" {
  port = 3000
  container { image = "front:1" }
}
deployment "api" {
  port = 8000
  container { image = "api:1" }
}
"#,
        );
        let locations = &state.services["web"].configuration.locations;
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].path, "/api");
        assert_eq!(locations[1].path, "/");
        assert_eq!(
            locations[1].target,
            HTTPLocationTarget::Instance {
                group: "frontend".into()
            }
        );

        let binding = state.deployments["frontend"]
            .service_binding
            .as_ref()
            .unwrap();
        assert_eq!(binding.service_name, "web");
        assert_eq!(binding.target_group, "frontend");
    }

    #[test]
    fn url_and_instance_group_targets_flow_through() {
        // `url` proxies externally (no binding); `instance_group` routes to a
        // raw group populated out-of-band (no binding either).
        let state = parse(
            r#"
project = "demo"
service "web" {
  location "/legacy" { url = "https://old.example.com" }
  location "/canary" { instance_group = "canary" }
}
"#,
        );
        let locations = &state.services["web"].configuration.locations;
        assert_eq!(locations.len(), 2);
        assert_eq!(
            locations[0].target,
            HTTPLocationTarget::Url {
                url: "https://old.example.com".into()
            }
        );
        assert_eq!(
            locations[1].target,
            HTTPLocationTarget::Instance {
                group: "canary".into()
            }
        );
        assert!(state.deployments.is_empty());
    }

    #[test]
    fn allow_http_and_override_404_flow_through() {
        let state = parse(
            r#"
project = "demo"
service "web" {
  allow_http = true
  location "/" {
    instance_group = "front"
    override_404   = "/index.html"
  }
}
"#,
        );
        let cfg = &state.services["web"].configuration;
        assert!(cfg.allow_http);
        assert_eq!(
            cfg.locations[0].override_404.as_deref(),
            Some("/index.html")
        );
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
    fn vcpus_attribute_sets_vcpu_count() {
        let state = parse(
            r#"
project = "demo"
deployment "api" {
  vcpus = 2
  container { image = "i:1" }
}
"#,
        );
        assert_eq!(state.deployments["api"].configuration.vcpu_count, 2);
    }

    #[test]
    fn bare_integer_memory_is_megabytes() {
        let state = parse(
            r#"
project = "demo"
deployment "api" {
  memory = 512
  container { image = "i:1" }
}
"#,
        );
        assert_eq!(state.deployments["api"].configuration.memory_mb, 512);
    }

    #[test]
    fn all_resource_attributes_flow_through_together() {
        let state = parse(
            r#"
project = "demo"
deployment "api" {
  vcpus      = 4
  vcpu_ratio = 0.125
  memory     = "2GB"
  replicas   = 2
  container { image = "i:1" }
}
"#,
        );
        let cfg = &state.deployments["api"].configuration;
        assert_eq!(cfg.vcpu_count, 4);
        assert_eq!(cfg.vcpu_ratio, 0.125);
        assert_eq!(cfg.memory_mb, 2048);
        assert_eq!(cfg.replicas, 2);
    }

    #[test]
    fn unspecified_memory_defaults_to_512mb() {
        let state = parse(
            r#"
project = "demo"
deployment "api" {
  container { image = "i:1" }
}
"#,
        );
        assert_eq!(state.deployments["api"].configuration.memory_mb, 512);
    }

    #[test]
    fn replicas_attribute_flows_through() {
        let state = parse(
            r#"
project = "demo"
deployment "api" {
  replicas = 3
  container { image = "i:1" }
}
"#,
        );
        assert_eq!(state.deployments["api"].configuration.replicas, 3);
    }

    #[test]
    fn vcpu_ratio_attribute_flows_through() {
        let state = parse(
            r#"
project = "demo"
deployment "api" {
  vcpu_ratio = 0.5
  container { image = "i:1" }
}
"#,
        );
        assert_eq!(state.deployments["api"].configuration.vcpu_ratio, 0.5);
    }

    #[test]
    fn memory_string_accepts_unit_variants() {
        // Case-insensitive MB/M/GB/G, binary units, fractional GB landing on
        // whole MB. Pins the accepted spellings.
        for (spec, mb) in [
            ("512MB", 512),
            ("512M", 512),
            ("2g", 2048),
            ("1gb", 1024),
            ("1.5GB", 1536),
        ] {
            let state = parse(&format!(
                r#"
project = "demo"
deployment "api" {{
  memory = "{spec}"
  container {{ image = "i:1" }}
}}
"#
            ));
            assert_eq!(
                state.deployments["api"].configuration.memory_mb, mb,
                "spec {spec:?}"
            );
        }
    }

    #[test]
    fn memory_string_with_gb_suffix_converts_to_mb() {
        let state = parse(
            r#"
project = "demo"
deployment "api" {
  memory = "1GB"
  container { image = "i:1" }
}
"#,
        );
        assert_eq!(state.deployments["api"].configuration.memory_mb, 1024);
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
