//! Render a [`Plan`] for the user. Colors via `console`, with summary line.

use std::fmt::Write;

use console::Style;

use super::diff;
use super::plan::{
    DeploymentAction, EnvAction, NetworkAction, Plan, RecreateReason, ServiceAction,
};

pub struct PlanStyles {
    pub add: Style,
    pub change: Style,
    pub destroy: Style,
    pub bold: Style,
    pub dim: Style,
}

impl PlanStyles {
    pub fn colored() -> Self {
        Self {
            add: Style::new().green(),
            change: Style::new().yellow(),
            destroy: Style::new().red(),
            bold: Style::new().bold(),
            dim: Style::new().dim(),
        }
    }

    pub fn plain() -> Self {
        Self {
            add: Style::new(),
            change: Style::new(),
            destroy: Style::new(),
            bold: Style::new(),
            dim: Style::new(),
        }
    }
}

pub fn render(plan: &Plan, styles: &PlanStyles) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Plan for project {}:",
        styles.bold.apply_to(&plan.project)
    );
    let _ = writeln!(out);

    // Counters for summary.
    let mut to_add = 0usize;
    let mut to_change = 0usize;
    let mut to_recreate = 0usize;
    let mut to_destroy = 0usize;

    // Env action.
    match &plan.env_action {
        EnvAction::Use(env) => {
            let _ = writeln!(
                out,
                "  {} environment {} (using existing)",
                styles.dim.apply_to("·"),
                styles.bold.apply_to(&env.name)
            );
        }
        EnvAction::Create(req) => {
            to_add += 1;
            let _ = writeln!(
                out,
                "  {} environment {}",
                styles.add.apply_to("+"),
                styles.bold.apply_to(&req.name)
            );
            if let Some(d) = &req.display_name {
                let _ = writeln!(out, "      display_name: {d:?}");
            }
            if let Some(d) = &req.description {
                if !d.is_empty() {
                    let _ = writeln!(out, "      description:  {d:?}");
                }
            }
        }
    }
    let _ = writeln!(out);

    // Networks (rendered first — apply creates them before anything that
    // references them).
    for action in &plan.network_actions {
        match action {
            NetworkAction::Create(n) => {
                to_add += 1;
                let _ = writeln!(
                    out,
                    "  {} network {}",
                    styles.add.apply_to("+"),
                    styles.bold.apply_to(&n.name)
                );
                let _ = writeln!(out, "      iprange: {}", n.ipv4_cidr);
            }
            NetworkAction::Recreate {
                current,
                desired,
                reasons,
            } => {
                to_recreate += 1;
                let _ = writeln!(
                    out,
                    "  {} network {} {}",
                    styles.destroy.apply_to("-/+"),
                    styles.bold.apply_to(&desired.name),
                    styles
                        .dim
                        .apply_to(format!("(recreate — {})", format_reasons(reasons)))
                );
                let _ = writeln!(
                    out,
                    "      iprange: {} -> {}",
                    current.ipv4_cidr, desired.ipv4_cidr
                );
            }
            NetworkAction::Delete(n) => {
                to_destroy += 1;
                let _ = writeln!(
                    out,
                    "  {} network {} {}",
                    styles.destroy.apply_to("-"),
                    styles.bold.apply_to(&n.name),
                    styles.dim.apply_to(format!("(iprange: {})", n.ipv4_cidr))
                );
            }
        }
    }

    // Services.
    for action in &plan.service_actions {
        match action {
            ServiceAction::Create(s) => {
                to_add += 1;
                let _ = writeln!(
                    out,
                    "  {} service {}",
                    styles.add.apply_to("+"),
                    styles.bold.apply_to(&s.name)
                );
                if !s.hosts.is_empty() {
                    let _ = writeln!(out, "      hosts:  {}", s.hosts.join(", "));
                }
                let _ = writeln!(out, "      region: {}", s.region);
            }
            ServiceAction::Update {
                desired, current, ..
            } => {
                to_change += 1;
                let _ = writeln!(
                    out,
                    "  {} service {}",
                    styles.change.apply_to("~"),
                    styles.bold.apply_to(&desired.name)
                );
                diff::service::render_config_diff(
                    &mut out,
                    &current.configuration,
                    &desired.configuration,
                );
            }
            ServiceAction::Recreate {
                current,
                desired,
                reasons,
            } => {
                to_recreate += 1;
                let _ = writeln!(
                    out,
                    "  {} service {} {}",
                    styles.destroy.apply_to("-/+"),
                    styles.bold.apply_to(&desired.name),
                    styles
                        .dim
                        .apply_to(format!("(recreate — {})", format_reasons(reasons)))
                );
                if diff::service::hosts_differ(desired, current) {
                    let _ = writeln!(
                        out,
                        "      hosts:  {} -> {}",
                        current.hosts.join(", "),
                        desired.hosts.join(", ")
                    );
                }
                if current.region != desired.region {
                    let _ = writeln!(
                        out,
                        "      region: {} -> {}",
                        current.region, desired.region
                    );
                }
            }
            ServiceAction::Delete(s) => {
                to_destroy += 1;
                let _ = writeln!(
                    out,
                    "  {} service {} {}",
                    styles.destroy.apply_to("-"),
                    styles.bold.apply_to(&s.name),
                    styles.dim.apply_to(if s.hosts.is_empty() {
                        "(base host only)".to_string()
                    } else {
                        format!("(hosts: {})", s.hosts.join(", "))
                    })
                );
            }
        }
    }

    // Deployments.
    for action in &plan.deployment_actions {
        match action {
            DeploymentAction::Create { desired: d, .. } => {
                to_add += 1;
                let _ = writeln!(
                    out,
                    "  {} deployment {}",
                    styles.add.apply_to("+"),
                    styles.bold.apply_to(&d.name)
                );
                let _ = writeln!(out, "      image:    {}", d.configuration.container_image);
                let _ = writeln!(out, "      replicas: {}", d.configuration.replicas);
                let _ = writeln!(out, "      region:   {}", d.configuration.region);
                if let Some(p) = d.configuration.instance_port {
                    let _ = writeln!(out, "      port:     {p}");
                }
                if let Some(b) = &d.service_binding {
                    let _ = writeln!(
                        out,
                        "      service:  {} (group={})",
                        b.service_name, b.target_group
                    );
                }
                if let Some(net) = &d.network {
                    let _ = writeln!(out, "      network:  {net}");
                }
            }
            DeploymentAction::Update {
                desired, current, ..
            } => {
                to_change += 1;
                let _ = writeln!(
                    out,
                    "  {} deployment {}",
                    styles.change.apply_to("~"),
                    styles.bold.apply_to(&desired.name)
                );
                diff::deployment::render_config_diff(
                    &mut out,
                    &current.configuration,
                    &desired.configuration,
                );
                render_network_transition(&mut out, current, desired);
            }
            DeploymentAction::Recreate {
                current,
                desired,
                reasons,
                ..
            } => {
                to_recreate += 1;
                let _ = writeln!(
                    out,
                    "  {} deployment {} {}",
                    styles.destroy.apply_to("-/+"),
                    styles.bold.apply_to(&desired.name),
                    styles
                        .dim
                        .apply_to(format!("(recreate — {})", format_reasons(reasons)))
                );
                if current.configuration.container_image != desired.configuration.container_image {
                    let _ = writeln!(
                        out,
                        "      image: {} -> {}",
                        current.configuration.container_image,
                        desired.configuration.container_image
                    );
                }
                // A recreate replaces the deployment wholesale, so a network
                // move/detach rides along — disclose it like Update does.
                render_network_transition(&mut out, current, desired);
            }
            DeploymentAction::Delete(d) => {
                to_destroy += 1;
                let _ = writeln!(
                    out,
                    "  {} deployment {} {}",
                    styles.destroy.apply_to("-"),
                    styles.bold.apply_to(&d.name),
                    styles
                        .dim
                        .apply_to(format!("(image: {})", d.configuration.container_image))
                );
            }
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Summary: {} to add, {} to change, {} to recreate, {} to destroy.",
        styles.add.apply_to(to_add),
        styles.change.apply_to(to_change),
        styles.destroy.apply_to(to_recreate),
        styles.destroy.apply_to(to_destroy),
    );

    out
}

/// Render a deployment's network-binding transition (`a -> b`), if any. The
/// binding lives outside the configuration blob, so the config diff can't
/// show it; both the Update and Recreate arms disclose it through this.
fn render_network_transition(
    out: &mut String,
    current: &crate::commands::up::plan::CurrentDeployment,
    desired: &crate::commands::up::desired::DesiredDeployment,
) {
    let c_net = current
        .network_binding
        .as_ref()
        .map(|b| b.network_name.as_str());
    let d_net = desired.network.as_deref();
    if c_net != d_net {
        let _ = writeln!(
            out,
            "      network:  {} -> {}",
            c_net.unwrap_or("<unset>"),
            d_net.unwrap_or("<unset>"),
        );
    }
}

fn format_reasons(reasons: &[RecreateReason]) -> String {
    reasons
        .iter()
        .map(|r| match r {
            RecreateReason::ImmutableField { field, .. } => format!("{field} changed"),
            RecreateReason::ServiceBindingChanged => "service binding changed".to_string(),
            RecreateReason::DependentServiceRecreated { service_name } => {
                format!("service {service_name} recreated")
            }
            RecreateReason::DependentNetworkRecreated { network_name } => {
                format!("network {network_name} recreated")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A service's reachable hosts after `up`: the always-live derived base host
/// plus any bound custom hosts.
pub struct Reachability {
    pub service: String,
    pub base_host: String,
    pub custom_hosts: Vec<String>,
}

/// Renders the post-`up` reachability summary: every service's base host
/// (immediately live via the wildcard cert) and its custom hosts as URLs.
pub fn render_reachability(services: &[Reachability]) -> String {
    let mut out = String::new();
    if services.is_empty() {
        return out;
    }
    let _ = writeln!(out, "\nReachable:");
    for svc in services {
        let _ = writeln!(out, "  {}", svc.service);
        let _ = writeln!(out, "      https://{}   (base)", svc.base_host);
        for host in &svc.custom_hosts {
            let _ = writeln!(out, "      https://{host}   (custom)");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::up::desired::{DesiredDeployment, DesiredService, DesiredServiceBinding};
    use crate::commands::up::plan::{
        CurrentService, CurrentServiceBinding, DeploymentAction, EnvAction, Plan, ServiceAction,
    };
    use std::collections::BTreeMap;
    use unisrv_api::models::{
        CreateEnvironmentRequest, DeploymentConfiguration, HTTPLocation, HTTPLocationTarget,
        HTTPServiceConfig,
    };
    use uuid::Uuid;

    fn http_config() -> HTTPServiceConfig {
        HTTPServiceConfig {
            allow_http: false,
            locations: vec![HTTPLocation {
                path: "/".into(),
                override_404: None,
                target: HTTPLocationTarget::Instance {
                    group: "default".into(),
                },
            }],
        }
    }

    fn dep_config(image: &str) -> DeploymentConfiguration {
        DeploymentConfiguration {
            replicas: 1,
            region: "dev".into(),
            container_image: image.into(),
            args: None,
            env: None,
            vcpu_ratio: 0.25,
            vcpu_count: 1,
            memory_mb: 256,
            instance_port: Some(80),
        }
    }

    #[test]
    fn renders_full_create_plan() {
        let plan = Plan {
            network_actions: vec![],
            project: "demo".into(),
            env_action: EnvAction::Create(CreateEnvironmentRequest {
                project: "demo".into(),
                name: "prod".into(),
                display_name: Some("Demo Production".into()),
                description: None,
            }),
            service_actions: vec![ServiceAction::Create(DesiredService {
                name: "web".into(),
                hosts: vec!["web.example".into()],
                region: "dev".into(),
                configuration: http_config(),
            })],
            deployment_actions: vec![DeploymentAction::Create {
                service: None,
                network: None,
                desired: DesiredDeployment {
                    network: None,
                    name: "web".into(),
                    configuration: dep_config("nginx:1"),
                    service_binding: Some(DesiredServiceBinding {
                        service_name: "web".into(),
                        target_group: "default".into(),
                    }),
                },
            }],
            instance_stops: vec![],
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(out.contains("+ environment prod"));
        assert!(out.contains("+ service web"));
        assert!(out.contains("+ deployment web"));
        assert!(out.contains("image:    nginx:1"));
        assert!(out.contains("3 to add, 0 to change, 0 to recreate, 0 to destroy."));
    }

    #[test]
    fn renders_recreate_with_reason() {
        let plan = Plan {
            network_actions: vec![],
            project: "demo".into(),
            env_action: EnvAction::Use(crate::commands::up::plan::ResolvedEnvironment {
                id: Uuid::new_v4(),
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![ServiceAction::Recreate {
                current: CurrentService {
                    id: Uuid::new_v4(),
                    name: "web".into(),
                    hosts: vec!["app.example".into()],
                    region: "dev".into(),
                    configuration: http_config(),
                },
                desired: DesiredService {
                    name: "web".into(),
                    hosts: vec!["app.example".into()],
                    region: "us-east".into(),
                    configuration: http_config(),
                },
                reasons: vec![RecreateReason::ImmutableField {
                    field: "region",
                    old: "dev".into(),
                    new: "us-east".into(),
                }],
            }],
            deployment_actions: vec![],
            instance_stops: vec![],
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(out.contains("-/+ service web"));
        assert!(out.contains("region changed"));
        assert!(out.contains("dev -> us-east"));
        assert!(out.contains("0 to add, 0 to change, 1 to recreate, 0 to destroy."));
    }

    #[test]
    fn recreate_omits_hosts_diff_when_sets_equal_but_reordered() {
        // Hosts are an unordered set. A recreate triggered by region change with
        // the same hosts in a different order must NOT render a spurious hosts diff.
        let plan = Plan {
            network_actions: vec![],
            project: "demo".into(),
            env_action: EnvAction::Use(crate::commands::up::plan::ResolvedEnvironment {
                id: Uuid::new_v4(),
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![ServiceAction::Recreate {
                current: CurrentService {
                    id: Uuid::new_v4(),
                    name: "web".into(),
                    hosts: vec!["b.com".into(), "a.com".into()],
                    region: "dev".into(),
                    configuration: http_config(),
                },
                desired: DesiredService {
                    name: "web".into(),
                    hosts: vec!["a.com".into(), "b.com".into()],
                    region: "us-east".into(),
                    configuration: http_config(),
                },
                reasons: vec![RecreateReason::ImmutableField {
                    field: "region",
                    old: "dev".into(),
                    new: "us-east".into(),
                }],
            }],
            deployment_actions: vec![],
            instance_stops: vec![],
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(
            !out.contains("hosts:"),
            "reordered-equal host sets must not render a hosts diff:\n{out}"
        );
    }

    #[test]
    fn renders_delete_summary() {
        let plan = Plan {
            network_actions: vec![],
            project: "demo".into(),
            env_action: EnvAction::Use(crate::commands::up::plan::ResolvedEnvironment {
                id: Uuid::new_v4(),
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Delete(
                crate::commands::up::plan::CurrentDeployment {
                    network_binding: None,
                    id: Uuid::new_v4(),
                    name: "old".into(),
                    configuration: dep_config("img:1"),
                    service_binding: None,
                },
            )],
            instance_stops: vec![],
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(out.contains("- deployment old"));
        assert!(out.contains("0 to add, 0 to change, 0 to recreate, 1 to destroy."));
    }

    #[test]
    fn renders_update_with_field_diff() {
        let _ = (CurrentServiceBinding {
            service_id: Uuid::new_v4(),
            service_name: "x".into(),
            target_group: "default".into(),
        },); // keep import live
        let _ = BTreeMap::<String, String>::new();
        let mut current_config = dep_config("nginx:1");
        let mut desired_config = dep_config("nginx:2");
        current_config.replicas = 1;
        desired_config.replicas = 3;

        let plan = Plan {
            network_actions: vec![],
            project: "demo".into(),
            env_action: EnvAction::Use(crate::commands::up::plan::ResolvedEnvironment {
                id: Uuid::new_v4(),
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Update {
                network: None,
                id: Uuid::new_v4(),
                desired: DesiredDeployment {
                    network: None,
                    name: "web".into(),
                    configuration: desired_config,
                    service_binding: None,
                },
                current: crate::commands::up::plan::CurrentDeployment {
                    network_binding: None,
                    id: Uuid::new_v4(),
                    name: "web".into(),
                    configuration: current_config,
                    service_binding: None,
                },
            }],
            instance_stops: vec![],
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(out.contains("~ deployment web"));
        assert!(out.contains("image:    nginx:1 -> nginx:2"));
        assert!(out.contains("replicas: 1 -> 3"));
        assert!(out.contains("0 to add, 1 to change, 0 to recreate, 0 to destroy."));
    }

    #[test]
    fn render_reachability_lists_base_and_custom_hosts() {
        let out = render_reachability(&[Reachability {
            service: "web".into(),
            base_host: "web-ab12.unisrv.dev".into(),
            custom_hosts: vec!["shop.acme.com".into()],
        }]);
        assert!(out.contains("web"));
        assert!(out.contains("https://web-ab12.unisrv.dev"));
        assert!(out.contains("(base)"));
        assert!(out.contains("https://shop.acme.com"));
        assert!(out.contains("(custom)"));
    }

    #[test]
    fn renders_network_actions_with_counts() {
        use crate::commands::up::desired::DesiredNetwork;
        use crate::commands::up::plan::{CurrentNetwork, NetworkAction};

        let plan = Plan {
            project: "demo".into(),
            env_action: EnvAction::Use(crate::commands::up::plan::ResolvedEnvironment {
                id: Uuid::new_v4(),
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![],
            deployment_actions: vec![],
            network_actions: vec![
                NetworkAction::Create(DesiredNetwork {
                    name: "internal".into(),
                    ipv4_cidr: "10.0.0.0/16".into(),
                }),
                NetworkAction::Recreate {
                    current: CurrentNetwork {
                        id: Uuid::new_v4(),
                        name: "backend".into(),
                        ipv4_cidr: "10.1.0.0/16".into(),
                    },
                    desired: DesiredNetwork {
                        name: "backend".into(),
                        ipv4_cidr: "10.2.0.0/24".into(),
                    },
                    reasons: vec![RecreateReason::ImmutableField {
                        field: "iprange",
                        old: "10.1.0.0/16".into(),
                        new: "10.2.0.0/24".into(),
                    }],
                },
                NetworkAction::Delete(CurrentNetwork {
                    id: Uuid::new_v4(),
                    name: "old".into(),
                    ipv4_cidr: "10.3.0.0/16".into(),
                }),
            ],
            instance_stops: vec![],
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(out.contains("+ network internal"), "got:\n{out}");
        assert!(out.contains("iprange: 10.0.0.0/16"), "got:\n{out}");
        assert!(out.contains("-/+ network backend"), "got:\n{out}");
        assert!(out.contains("iprange changed"), "got:\n{out}");
        assert!(
            out.contains("10.1.0.0/16 -> 10.2.0.0/24"),
            "shows the cidr transition: {out}"
        );
        assert!(out.contains("- network old"), "got:\n{out}");
        assert!(
            out.contains("1 to add, 0 to change, 1 to recreate, 1 to destroy."),
            "got:\n{out}"
        );
    }

    #[test]
    fn deployment_create_and_update_render_network_binding() {
        use crate::commands::up::plan::CurrentNetworkBinding;

        let plan = Plan {
            project: "demo".into(),
            env_action: EnvAction::Use(crate::commands::up::plan::ResolvedEnvironment {
                id: Uuid::new_v4(),
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![],
            deployment_actions: vec![
                DeploymentAction::Create {
                    service: None,
                    network: None,
                    desired: DesiredDeployment {
                        name: "api".into(),
                        configuration: dep_config("i:1"),
                        service_binding: None,
                        network: Some("internal".into()),
                    },
                },
                DeploymentAction::Update {
                    network: None,
                    id: Uuid::new_v4(),
                    desired: DesiredDeployment {
                        name: "worker".into(),
                        configuration: dep_config("w:1"),
                        service_binding: None,
                        network: Some("internal".into()),
                    },
                    current: crate::commands::up::plan::CurrentDeployment {
                        id: Uuid::new_v4(),
                        name: "worker".into(),
                        configuration: dep_config("w:1"),
                        service_binding: None,
                        network_binding: Some(CurrentNetworkBinding {
                            network_id: Uuid::new_v4(),
                            network_name: "backend".into(),
                        }),
                    },
                },
            ],
            network_actions: vec![],
            instance_stops: vec![],
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(
            out.contains("network:  internal"),
            "create shows network: {out}"
        );
        assert!(
            out.contains("network:  backend -> internal"),
            "update shows binding change: {out}"
        );
    }

    #[test]
    fn deployment_recreate_renders_network_transition() {
        // A recreate replaces the deployment wholesale, so a network move or
        // detach rides along — the plan must disclose it, same as Update does.
        use crate::commands::up::plan::CurrentNetworkBinding;

        let plan = Plan {
            project: "demo".into(),
            env_action: EnvAction::Use(crate::commands::up::plan::ResolvedEnvironment {
                id: Uuid::new_v4(),
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Recreate {
                network: None,
                service: None,
                current: crate::commands::up::plan::CurrentDeployment {
                    id: Uuid::new_v4(),
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network_binding: Some(CurrentNetworkBinding {
                        network_id: Uuid::new_v4(),
                        network_name: "internal".into(),
                    }),
                },
                desired: DesiredDeployment {
                    name: "api".into(),
                    configuration: dep_config("i:1"),
                    service_binding: None,
                    network: None, // detached as part of the recreate
                },
                reasons: vec![RecreateReason::ServiceBindingChanged],
            }],
            network_actions: vec![],
            instance_stops: vec![],
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(
            out.contains("network:  internal -> <unset>"),
            "recreate must disclose the network transition: {out}"
        );
    }

    #[test]
    fn render_reachability_empty_is_blank() {
        assert_eq!(render_reachability(&[]), "");
    }
}
