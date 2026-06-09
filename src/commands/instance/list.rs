//! `unisrv instance ls` — tabulate an environment's instances.

use anyhow::Result;
use chrono::NaiveDateTime;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table, presets::UTF8_FULL};
use unisrv_api::ApiClient;
use unisrv_api::models::{InstanceListEntry, InstanceListResponse};

use crate::commands::ui::{cell_with_color, colors_enabled, format_relative};
use crate::commands::up::plan::ResolvedEnvironment;

/// List the instances of `env`. Hides stopped instances unless `all`; emits the
/// (filtered) list as JSON when `json`, otherwise a human table.
pub async fn list(
    client: &dyn ApiClient,
    env: &ResolvedEnvironment,
    all: bool,
    json: bool,
) -> Result<()> {
    let resp = client.list_instances(env.id).await?;
    let shown = filter(resp.instances, all);

    if json {
        let payload = InstanceListResponse { instances: shown };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if shown.is_empty() {
        if all {
            println!("No instances in environment {}.", env.name);
        } else {
            println!(
                "No active instances in environment {} (pass -a to include stopped ones).",
                env.name
            );
        }
        return Ok(());
    }

    let use_color = colors_enabled();
    let now = chrono::Utc::now().naive_utc();
    println!("{}", render_table(&shown, now, use_color));
    Ok(())
}

/// States considered "live". Everything else (exited, failed, stopped, …) is
/// hidden unless `--all` is given, mirroring `docker ps`.
fn is_active(state: &str) -> bool {
    matches!(state, "running" | "provisioning")
}

/// Keep only the instances to display: all of them with `all`, otherwise just
/// the active ones.
fn filter(instances: Vec<InstanceListEntry>, all: bool) -> Vec<InstanceListEntry> {
    instances
        .into_iter()
        .filter(|i| all || is_active(&i.state.0))
        .collect()
}

/// Render the instances as a bordered table. Pure so it can be asserted on
/// without a terminal; colour is gated by the caller.
fn render_table(instances: &[InstanceListEntry], now: NaiveDateTime, use_color: bool) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        Cell::new("ID").add_attribute(Attribute::Bold),
        Cell::new("NAME").add_attribute(Attribute::Bold),
        Cell::new("IMAGE").add_attribute(Attribute::Bold),
        Cell::new("STATE").add_attribute(Attribute::Bold),
        Cell::new("DEPLOYMENT").add_attribute(Attribute::Bold),
        Cell::new("CREATED").add_attribute(Attribute::Bold),
    ]);

    for instance in instances {
        let short_id = instance.id.to_string()[..8].to_string();
        let (name, name_color) = match instance.name.as_deref() {
            Some(n) => (n.to_string(), None),
            None => ("\u{2014}".to_string(), Some(Color::DarkGrey)),
        };
        let (state_text, state_color) = format_state(&instance.state.0);
        let (deployment, deployment_color) = match &instance.deployment {
            Some(d) => (d.name.clone(), None),
            None => ("\u{2014}".to_string(), Some(Color::DarkGrey)),
        };
        let created = format_relative(instance.created_at, now);

        table.add_row(vec![
            Cell::new(short_id),
            cell_with_color(name, name_color, use_color),
            Cell::new(&instance.container_image),
            cell_with_color(state_text, state_color, use_color),
            cell_with_color(deployment, deployment_color, use_color),
            Cell::new(created),
        ]);
    }
    table.to_string()
}

/// State → (display, colour): live states green/yellow, terminal states dimmed,
/// failures red. Unknown states render plainly so a new backend state still
/// shows up readably.
fn format_state(state: &str) -> (String, Option<Color>) {
    let color = match state {
        "running" => Some(Color::Green),
        "provisioning" | "starting" | "creating" => Some(Color::Yellow),
        "failed" | "error" | "crashed" => Some(Color::Red),
        "stopped" | "stopping" | "exited" | "terminated" => Some(Color::DarkGrey),
        _ => None,
    };
    (state.to_string(), color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unisrv_api::ApiError;
    use unisrv_api::models::{DeploymentInfo, InstanceState};
    use unisrv_api::test_support::MockApiClient;
    use uuid::Uuid;

    fn env() -> ResolvedEnvironment {
        ResolvedEnvironment {
            id: Uuid::new_v4(),
            name: "prod".to_string(),
            project: "demo".to_string(),
            slug: "ab12".to_string(),
        }
    }

    fn instance(name: &str, state: &str) -> InstanceListEntry {
        InstanceListEntry {
            id: Uuid::new_v4(),
            name: Some(name.to_string()),
            state: InstanceState(state.to_string()),
            container_image: "nginx:latest".to_string(),
            created_at: NaiveDateTime::default(),
            deployment: None,
        }
    }

    #[test]
    fn filter_hides_stopped_by_default() {
        let instances = vec![
            instance("web", "running"),
            instance("old", "exited"),
            instance("boot", "provisioning"),
        ];
        let shown = filter(instances, false);
        let names: Vec<&str> = shown.iter().filter_map(|i| i.name.as_deref()).collect();
        assert_eq!(
            names,
            vec!["web", "boot"],
            "exited instance should be hidden"
        );
    }

    #[test]
    fn filter_all_keeps_everything() {
        let instances = vec![instance("web", "running"), instance("old", "exited")];
        assert_eq!(filter(instances, true).len(), 2);
    }

    #[test]
    fn render_table_has_columns_and_marks_standalone_with_dash() {
        let now = NaiveDateTime::default();
        let mut deployed = instance("api-0", "running");
        deployed.deployment = Some(DeploymentInfo {
            id: Uuid::new_v4(),
            name: "api".to_string(),
        });
        let standalone = instance("scratch", "running");

        let rendered = render_table(&[deployed, standalone], now, false);

        for header in ["ID", "NAME", "IMAGE", "STATE", "DEPLOYMENT", "CREATED"] {
            assert!(
                rendered.contains(header),
                "missing column {header}:\n{rendered}"
            );
        }
        assert!(rendered.contains("api-0"));
        assert!(rendered.contains("scratch"));
        assert!(rendered.contains("api"), "deployment name should show");
        assert!(
            rendered.contains('\u{2014}'),
            "standalone deployment should be an em dash"
        );
    }

    #[tokio::test]
    async fn list_queries_the_selected_environment() {
        let env = env();
        let mock = MockApiClient::logged_in().with_list_instances(Ok(InstanceListResponse {
            instances: vec![instance("web", "running")],
        }));

        let result = list(&mock, &env, false, false).await;

        assert!(result.is_ok(), "expected ok, got {result:?}");
        assert_eq!(
            mock.calls.lock().unwrap().list_instances_calls,
            vec![env.id]
        );
    }

    #[tokio::test]
    async fn list_json_renders_without_error() {
        let mock = MockApiClient::logged_in()
            .with_list_instances(Ok(InstanceListResponse { instances: vec![] }));
        assert!(list(&mock, &env(), false, true).await.is_ok());
    }

    #[tokio::test]
    async fn list_propagates_api_error() {
        let mock = MockApiClient::logged_in().with_list_instances(Err(ApiError::Server {
            status: 500,
            reason: "boom".into(),
        }));
        let err = list(&mock, &env(), false, false).await.unwrap_err();
        assert!(err.to_string().contains("500"));
    }
}
