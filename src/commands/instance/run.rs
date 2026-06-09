//! Entry point for the `instance` command group: resolve the environment
//! (manifest → project → remembered/picked env), announce it, then dispatch to
//! the list or logs handler.

use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use unisrv_api::ApiClient;
use unisrv_api::models::EnvironmentListEntry;

use super::select_env::{EnvPicker, select_environment};
use super::{list, logs};
use crate::commands::up::config::UpConfig;
use crate::config_locate::{CONFIG_FILE, find_config};
use crate::preferences::{FilePreferenceStore, NullPreferenceStore, PreferenceStore};

/// What the user asked the instance group to do.
pub enum InstanceAction {
    List { all: bool, json: bool },
    Logs { reference: String, follow: bool },
}

/// Resolve the target environment and run `action` against it. `env_flag` is the
/// optional `--env <name>` from the subcommand.
pub async fn run(
    client: &dyn ApiClient,
    env_flag: Option<&str>,
    action: InstanceAction,
) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine the current directory")?;
    let manifest = find_config(&cwd, CONFIG_FILE);
    let project = match &manifest {
        Some(m) => Some(UpConfig::load_project(&m.path)?),
        None => None,
    };
    // Remembered choices are keyed by the project root (or the CWD when there's
    // no manifest to anchor to).
    let pref_dir = manifest.as_ref().map(|m| m.dir.clone()).unwrap_or(cwd);

    // Remembered choices live next to the auth store. With no home directory to
    // persist to, remember nothing rather than scatter state into a shared temp
    // file — we simply re-prompt next time.
    let mut prefs: Box<dyn PreferenceStore> = match FilePreferenceStore::default_path() {
        Some(path) => Box::new(FilePreferenceStore::new(path)),
        None => Box::new(NullPreferenceStore),
    };
    let picker = DialoguerEnvPicker;

    let env = select_environment(
        client,
        project.as_deref(),
        &pref_dir,
        env_flag,
        prefs.as_mut(),
        &picker,
    )
    .await?;

    // Always tell the user which environment we landed on — but keep stdout
    // clean for machine output, so the banner goes to stderr and is skipped
    // entirely for `--json`.
    let json = matches!(action, InstanceAction::List { json: true, .. });
    if !json {
        eprintln!(
            "{}",
            console::style(format!("→ env: {} (project {})", env.name, env.project)).dim()
        );
    }

    match action {
        InstanceAction::List { all, json } => list::list(client, &env, all, json).await,
        InstanceAction::Logs { reference, follow } => {
            logs::logs(client, &env, &reference, follow).await
        }
    }
}

/// Production environment picker: a dialoguer select that refuses to guess when
/// there's no terminal to prompt at.
struct DialoguerEnvPicker;

impl EnvPicker for DialoguerEnvPicker {
    fn pick(&self, candidates: &[EnvironmentListEntry]) -> Result<EnvironmentListEntry> {
        if !std::io::stdin().is_terminal() {
            bail!(
                "multiple environments to choose from; re-run with --env <name> (no terminal available to prompt)"
            );
        }
        let items: Vec<String> = candidates
            .iter()
            .map(|e| format!("{} (project {})", e.name, e.project))
            .collect();
        let index = dialoguer::Select::new()
            .with_prompt("Select an environment")
            .items(&items)
            .default(0)
            .interact()
            .context("failed to read environment selection")?;
        Ok(candidates[index].clone())
    }
}
