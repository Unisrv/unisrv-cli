//! Top-level orchestration for `unisrv up`.
//!
//! Composition only — each step lives in its own module with focused tests.

use anyhow::{Context, Result, bail};
use dialoguer::{Confirm, Input};
use std::collections::BTreeSet;
use std::path::Path;
use unisrv_api::ApiClient;

use super::apply::apply;
use super::config::UpConfig;
use super::desired::DesiredState;
use super::env_resolve::{Prompter, resolve as resolve_env};
use super::fetch::fetch_current_state;
use super::plan::{EnvAction, diff};
use super::preflight::validate_hosts_against;
use super::render::{PlanStyles, render};

const CONFIG_FILE: &str = "unisrv.hcl";

pub async fn run(client: &dyn ApiClient, env_flag: Option<&str>) -> Result<()> {
    let path = Path::new(CONFIG_FILE);
    if !path.exists() {
        bail!("no {CONFIG_FILE} found in current directory");
    }
    let config = UpConfig::load(path)?;
    let desired = DesiredState::from_config(config);

    let hosts = client.list_hosts().await?;
    let referenced: BTreeSet<&str> = desired.services.values().map(|s| s.host.as_str()).collect();
    validate_hosts_against(&referenced, &hosts)?;

    let prompter = DialoguerPrompter;
    let env_action = resolve_env(client, &desired.project, env_flag, &prompter).await?;

    // If we're creating an env, there is no current state to fetch.
    let current = match &env_action {
        EnvAction::Use(env) => fetch_current_state(client, env.id, &hosts).await?,
        EnvAction::Create(_) => super::plan::CurrentState::empty(),
    };

    let plan = diff(&desired, &current, env_action);

    if plan.is_empty() {
        println!("No changes.");
        return Ok(());
    }

    let styles = if console::Term::stdout().features().colors_supported() {
        PlanStyles::colored()
    } else {
        PlanStyles::plain()
    };
    print!("{}", render(&plan, &styles));

    let confirmed = Confirm::new()
        .with_prompt("Apply these changes?")
        .default(false)
        .interact()
        .context("failed to read confirmation")?;
    if !confirmed {
        println!("Aborted.");
        return Ok(());
    }

    apply(plan, client).await?;
    Ok(())
}

struct DialoguerPrompter;

impl Prompter for DialoguerPrompter {
    fn prompt_string(&self, prompt: &str, default: Option<&str>) -> Result<String> {
        let mut input = Input::<String>::new().with_prompt(prompt).allow_empty(true);
        if let Some(d) = default {
            input = input.default(d.to_string());
        }
        let value = input.interact_text()?;
        Ok(value)
    }
    fn prompt_optional(&self, prompt: &str) -> Result<Option<String>> {
        let value: String = Input::new()
            .with_prompt(prompt)
            .allow_empty(true)
            .default(String::new())
            .interact_text()?;
        if value.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }
}
