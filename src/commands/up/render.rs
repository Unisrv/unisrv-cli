//! Render a [`Plan`] for the user. Colors via `console`, with summary line.

use std::fmt::Write;

use console::Style;

use super::diff;
use super::plan::{DeploymentAction, EnvAction, Plan, RecreateReason, ServiceAction};

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
            DeploymentAction::Create(d) => {
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
            }
            DeploymentAction::Recreate {
                current,
                desired,
                reasons,
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

fn format_reasons(reasons: &[RecreateReason]) -> String {
    reasons
        .iter()
        .map(|r| match r {
            RecreateReason::ImmutableField { field, .. } => format!("{field} changed"),
            RecreateReason::ServiceBindingChanged => "service binding changed".to_string(),
            RecreateReason::DependentServiceRecreated { service_name } => {
                format!("service {service_name} recreated")
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
            network: None,
            instance_port: Some(80),
        }
    }

    #[test]
    fn renders_full_create_plan() {
        let plan = Plan {
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
            deployment_actions: vec![DeploymentAction::Create(DesiredDeployment {
                name: "web".into(),
                configuration: dep_config("nginx:1"),
                service_binding: Some(DesiredServiceBinding {
                    service_name: "web".into(),
                    target_group: "default".into(),
                }),
            })],
            existing_service_ids: BTreeMap::new(),
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
            existing_service_ids: BTreeMap::new(),
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
            existing_service_ids: BTreeMap::new(),
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
                    id: Uuid::new_v4(),
                    name: "old".into(),
                    configuration: dep_config("img:1"),
                    service_binding: None,
                },
            )],
            existing_service_ids: BTreeMap::new(),
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
            project: "demo".into(),
            env_action: EnvAction::Use(crate::commands::up::plan::ResolvedEnvironment {
                id: Uuid::new_v4(),
                name: "prod".into(),
                project: "demo".into(),
                slug: "ab12".into(),
            }),
            service_actions: vec![],
            deployment_actions: vec![DeploymentAction::Update {
                id: Uuid::new_v4(),
                desired: DesiredDeployment {
                    name: "web".into(),
                    configuration: desired_config,
                    service_binding: None,
                },
                current: crate::commands::up::plan::CurrentDeployment {
                    id: Uuid::new_v4(),
                    name: "web".into(),
                    configuration: current_config,
                    service_binding: None,
                },
            }],
            existing_service_ids: BTreeMap::new(),
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
    fn render_reachability_empty_is_blank() {
        assert_eq!(render_reachability(&[]), "");
    }
}
