//! Render a [`Plan`] for the user. Colors via `console`, with summary line.

use std::fmt::Write;

use console::Style;
use unisrv_api::models::{DeploymentConfiguration, HTTPServiceConfig};

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
    let _ = writeln!(out, "Plan for project {}:", styles.bold.apply_to(&plan.project));
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
                let _ = writeln!(out, "      host:   {}", s.host);
                let _ = writeln!(out, "      region: {}", s.region);
            }
            ServiceAction::Update { desired, current, .. } => {
                to_change += 1;
                let _ = writeln!(
                    out,
                    "  {} service {}",
                    styles.change.apply_to("~"),
                    styles.bold.apply_to(&desired.name)
                );
                render_service_config_diff(&mut out, &current.configuration, &desired.configuration);
            }
            ServiceAction::Recreate { current, desired, reasons } => {
                to_recreate += 1;
                let _ = writeln!(
                    out,
                    "  {} service {} {}",
                    styles.destroy.apply_to("-/+"),
                    styles.bold.apply_to(&desired.name),
                    styles.dim.apply_to(format!("(recreate — {})", format_reasons(reasons)))
                );
                if current.host != desired.host {
                    let _ = writeln!(out, "      host:   {} -> {}", current.host, desired.host);
                }
                if current.region != desired.region {
                    let _ = writeln!(out, "      region: {} -> {}", current.region, desired.region);
                }
            }
            ServiceAction::Delete(s) => {
                to_destroy += 1;
                let _ = writeln!(
                    out,
                    "  {} service {} {}",
                    styles.destroy.apply_to("-"),
                    styles.bold.apply_to(&s.name),
                    styles.dim.apply_to(format!("(host: {})", s.host))
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
                    let _ = writeln!(out, "      service:  {} (group={})", b.service_name, b.target_group);
                }
            }
            DeploymentAction::Update { desired, current, .. } => {
                to_change += 1;
                let _ = writeln!(
                    out,
                    "  {} deployment {}",
                    styles.change.apply_to("~"),
                    styles.bold.apply_to(&desired.name)
                );
                render_deployment_config_diff(&mut out, &current.configuration, &desired.configuration);
            }
            DeploymentAction::Recreate { current, desired, reasons } => {
                to_recreate += 1;
                let _ = writeln!(
                    out,
                    "  {} deployment {} {}",
                    styles.destroy.apply_to("-/+"),
                    styles.bold.apply_to(&desired.name),
                    styles.dim.apply_to(format!("(recreate — {})", format_reasons(reasons)))
                );
                if current.configuration.container_image != desired.configuration.container_image {
                    let _ = writeln!(
                        out,
                        "      image: {} -> {}",
                        current.configuration.container_image, desired.configuration.container_image
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
                    styles.dim.apply_to(format!("(image: {})", d.configuration.container_image))
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

fn render_service_config_diff(
    out: &mut String,
    current: &HTTPServiceConfig,
    desired: &HTTPServiceConfig,
) {
    if current.allow_http != desired.allow_http {
        let _ = writeln!(
            out,
            "      allow_http: {} -> {}",
            current.allow_http, desired.allow_http
        );
    }
    if current.locations != desired.locations {
        let _ = writeln!(out, "      locations:  (changed)");
    }
}

fn render_deployment_config_diff(
    out: &mut String,
    current: &DeploymentConfiguration,
    desired: &DeploymentConfiguration,
) {
    if current.container_image != desired.container_image {
        let _ = writeln!(
            out,
            "      image:    {} -> {}",
            current.container_image, desired.container_image
        );
    }
    if current.replicas != desired.replicas {
        let _ = writeln!(out, "      replicas: {} -> {}", current.replicas, desired.replicas);
    }
    if current.region != desired.region {
        let _ = writeln!(out, "      region:   {} -> {}", current.region, desired.region);
    }
    if current.instance_port != desired.instance_port {
        let _ = writeln!(
            out,
            "      port:     {:?} -> {:?}",
            current.instance_port, desired.instance_port
        );
    }
    if current.args != desired.args {
        let _ = writeln!(out, "      args:     (changed)");
    }
    if current.env != desired.env {
        let _ = writeln!(out, "      env:      (changed)");
    }
    if current.vcpu_count != desired.vcpu_count
        || current.vcpu_ratio != desired.vcpu_ratio
        || current.memory_mb != desired.memory_mb
    {
        let _ = writeln!(
            out,
            "      resources: {}vcpu @ {} / {}MB -> {}vcpu @ {} / {}MB",
            current.vcpu_count, current.vcpu_ratio, current.memory_mb,
            desired.vcpu_count, desired.vcpu_ratio, desired.memory_mb,
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
        })
        .collect::<Vec<_>>()
        .join(", ")
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
                host: "web.example".into(),
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
            env_action: EnvAction::Use(unisrv_api::models::EnvironmentResponse {
                id: Uuid::new_v4(),
                project: "demo".into(),
                name: "prod".into(),
                display_name: None,
                description: None,
                created_at: chrono::NaiveDateTime::default(),
                updated_at: chrono::NaiveDateTime::default(),
            }),
            service_actions: vec![ServiceAction::Recreate {
                current: CurrentService {
                    id: Uuid::new_v4(),
                    name: "web".into(),
                    host: "old.example".into(),
                    region: "dev".into(),
                    configuration: http_config(),
                },
                desired: DesiredService {
                    name: "web".into(),
                    host: "new.example".into(),
                    region: "dev".into(),
                    configuration: http_config(),
                },
                reasons: vec![RecreateReason::ImmutableField {
                    field: "host",
                    old: "old.example".into(),
                    new: "new.example".into(),
                }],
            }],
            deployment_actions: vec![],
            existing_service_ids: BTreeMap::new(),
        };
        let out = render(&plan, &PlanStyles::plain());
        assert!(out.contains("-/+ service web"));
        assert!(out.contains("host changed"));
        assert!(out.contains("old.example -> new.example"));
        assert!(out.contains("0 to add, 0 to change, 1 to recreate, 0 to destroy."));
    }

    #[test]
    fn renders_delete_summary() {
        let plan = Plan {
            project: "demo".into(),
            env_action: EnvAction::Use(unisrv_api::models::EnvironmentResponse {
                id: Uuid::new_v4(),
                project: "demo".into(),
                name: "prod".into(),
                display_name: None,
                description: None,
                created_at: chrono::NaiveDateTime::default(),
                updated_at: chrono::NaiveDateTime::default(),
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
            env_action: EnvAction::Use(unisrv_api::models::EnvironmentResponse {
                id: Uuid::new_v4(),
                project: "demo".into(),
                name: "prod".into(),
                display_name: None,
                description: None,
                created_at: chrono::NaiveDateTime::default(),
                updated_at: chrono::NaiveDateTime::default(),
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
}
