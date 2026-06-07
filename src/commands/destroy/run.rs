//! Top-level orchestration for `unisrv destroy`.
//!
//! Destroy is `up` run against an *empty* desired state (project name only): the
//! diff against the live environment becomes all-deletes. We then tear down any
//! standalone instances and delete the now-empty environment itself.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use dialoguer::Confirm;
use unisrv_api::ApiClient;

use super::execute::{RealWaiter, destroy_execute};
use super::resolve::resolve_for_destroy;
use super::stops::select_instance_stops;
use crate::commands::up::config::UpConfig;
use crate::commands::up::desired::DesiredState;
use crate::commands::up::fetch::fetch_current_state;
use crate::commands::up::plan::{EnvAction, diff};
use crate::commands::up::render::{PlanStyles, render};

const CONFIG_FILE: &str = "unisrv.hcl";

pub async fn run(client: &dyn ApiClient, env_flag: Option<&str>) -> Result<()> {
    let path = Path::new(CONFIG_FILE);
    if !path.exists() {
        anyhow::bail!("no {CONFIG_FILE} found in current directory");
    }
    let config = UpConfig::load(path)?;
    let project = config.project;

    let Some(env) = resolve_for_destroy(client, &project, env_flag).await? else {
        println!("Nothing to destroy: no environment found for project {project:?}.");
        return Ok(());
    };

    // Empty desired state → the diff deletes every live service and deployment.
    let desired = DesiredState {
        project: project.clone(),
        services: BTreeMap::new(),
        deployments: BTreeMap::new(),
    };
    let current = fetch_current_state(client, env.id).await?;

    // Standalone instances aren't modelled in the diff; tear them down explicitly.
    let instances = client.list_instances(env.id).await?;
    let instance_stops = select_instance_stops(&instances.instances);

    let env_name = env.name.clone();
    let mut plan = diff(&desired, &current, EnvAction::Use(env));
    plan.instance_stops = instance_stops;

    // Render what's about to be destroyed (env shell included), then confirm.
    let styles = if console::Term::stdout().features().colors_supported() {
        PlanStyles::colored()
    } else {
        PlanStyles::plain()
    };
    println!("Destroying environment {env_name:?} (project {project:?}):\n");
    print!("{}", render(&plan, &styles));
    for stop in &plan.instance_stops {
        println!(
            "  - instance {} (standalone) will be stopped",
            stop.name.as_deref().unwrap_or("<unnamed>")
        );
    }
    println!("  - environment {env_name} will be deleted");

    let confirmed = Confirm::new()
        .with_prompt(format!(
            "Destroy environment {env_name:?}? This permanently deletes everything in it and cannot be undone."
        ))
        .default(false)
        // Don't re-print the prompt+answer after confirming (dialoguer's default
        // "report"): the long destroy prompt doubled on screen is just noise.
        .report(false)
        .interact()
        .context("failed to read confirmation")?;
    if !confirmed {
        println!("Aborted.");
        return Ok(());
    }

    // Destroy never links/unlinks hosts (deletes free them via cascade), so apply
    // needs no claimed-host list.
    destroy_execute(plan, client, &[], &RealWaiter).await?;
    Ok(())
}
