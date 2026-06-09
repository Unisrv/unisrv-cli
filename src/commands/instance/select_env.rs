//! Decide which environment an instance command operates on.
//!
//! Unlike `up`, instance commands never create an environment — they only ever
//! select an existing one. The rules:
//!
//! * **`--env <name>`** pins by name and is always ephemeral (never remembered).
//!   In a project it matches within that project; with no manifest it matches
//!   across all environments, and a name shared by several projects opens the
//!   picker.
//! * **No flag, single candidate** → use it.
//! * **No flag, several candidates** → reuse the directory's remembered choice
//!   if it still exists, otherwise open the picker and remember the pick.
//! * **No candidates** → error (point the user at `unisrv up`).
//!
//! The candidate set is the project's environments when a manifest defines one,
//! or every environment when there is no manifest. Interactive selection is
//! behind [`EnvPicker`] so tests can script it and a non-interactive run can
//! fail cleanly instead of hanging.

use std::path::Path;

use anyhow::{Result, bail};
use unisrv_api::ApiClient;
use unisrv_api::models::EnvironmentListEntry;

use crate::commands::up::plan::ResolvedEnvironment;
use crate::preferences::{EnvRef, PreferenceStore};

/// Interactive chooser over candidate environments. Production uses a
/// dialoguer select that errors when there's no TTY; tests script the choice.
pub trait EnvPicker {
    fn pick(&self, candidates: &[EnvironmentListEntry]) -> Result<EnvironmentListEntry>;
}

/// Select the environment to act on. See the module docs for the rules.
pub async fn select_environment(
    client: &dyn ApiClient,
    project: Option<&str>,
    pref_dir: &Path,
    env_flag: Option<&str>,
    prefs: &mut dyn PreferenceStore,
    picker: &dyn EnvPicker,
) -> Result<ResolvedEnvironment> {
    let all = client.list_environments().await?.environments;
    let candidates: Vec<EnvironmentListEntry> = match project {
        Some(p) => all.into_iter().filter(|e| e.project == p).collect(),
        None => all,
    };

    // `--env` is an explicit, one-off override: matched by name, never persisted.
    if let Some(name) = env_flag {
        let named: Vec<EnvironmentListEntry> =
            candidates.into_iter().filter(|e| e.name == name).collect();
        return match named.as_slice() {
            [only] => Ok(ResolvedEnvironment::from(only)),
            [] => bail!("{}", no_match_message(project, name)),
            _ => Ok(ResolvedEnvironment::from(&picker.pick(&named)?)),
        };
    }

    match candidates.as_slice() {
        [] => bail!("{}", no_environments_message(project)),
        [only] => Ok(ResolvedEnvironment::from(only)),
        many => {
            // Reuse the remembered choice when it still exists; the id is
            // authoritative, so a deleted/recreated env falls through to a pick.
            if let Some(remembered) = prefs.get(pref_dir)
                && let Some(found) = many.iter().find(|e| e.id == remembered.env_id)
            {
                return Ok(ResolvedEnvironment::from(found));
            }

            let chosen = picker.pick(many)?;
            // Remembering the pick is best-effort UX state (see preferences.rs):
            // a write failure (read-only home, full disk) must not fail an
            // otherwise-successful selection. Warn and carry on.
            if let Err(e) = prefs.set(
                pref_dir,
                EnvRef {
                    env_id: chosen.id,
                    env_name: chosen.name.clone(),
                    project: chosen.project.clone(),
                },
            ) {
                eprintln!(
                    "{}",
                    console::style(format!("note: couldn't remember environment choice: {e}"))
                        .dim()
                );
            }
            Ok(ResolvedEnvironment::from(&chosen))
        }
    }
}

fn no_match_message(project: Option<&str>, name: &str) -> String {
    match project {
        Some(p) => format!(
            "no environment named {name:?} for project {p:?}. Run `unisrv up` to create it."
        ),
        None => format!("no environment named {name:?}."),
    }
}

fn no_environments_message(project: Option<&str>) -> String {
    match project {
        Some(p) => {
            format!("no environments exist for project {p:?}. Run `unisrv up` to create one.")
        }
        None => "you have no environments yet. Run `unisrv up` in a project to create one.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;

    use chrono::NaiveDateTime;
    use unisrv_api::models::EnvironmentListResponse;
    use unisrv_api::test_support::MockApiClient;
    use uuid::Uuid;

    use crate::preferences::FilePreferenceStore;

    fn entry(name: &str, project: &str) -> EnvironmentListEntry {
        EnvironmentListEntry {
            id: Uuid::new_v4(),
            project: project.to_string(),
            name: name.to_string(),
            slug: format!("{name}-slug"),
            display_name: None,
            description: None,
            instance_count: 0,
            service_count: 0,
            deployment_count: 0,
            network_count: 0,
            created_at: NaiveDateTime::default(),
        }
    }

    fn client(entries: Vec<EnvironmentListEntry>) -> MockApiClient {
        MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
            environments: entries,
        }))
    }

    /// Picker that fails the test if it's ever consulted.
    struct NoPicker;
    impl EnvPicker for NoPicker {
        fn pick(&self, _: &[EnvironmentListEntry]) -> Result<EnvironmentListEntry> {
            panic!("picker must not be called");
        }
    }

    /// Picker that returns a fixed candidate index (or errors, mimicking no TTY),
    /// counting how often it was consulted.
    struct ScriptedPicker {
        choice: Option<usize>,
        calls: Cell<u32>,
    }
    impl ScriptedPicker {
        fn picks(index: usize) -> Self {
            Self {
                choice: Some(index),
                calls: Cell::new(0),
            }
        }
        fn unavailable() -> Self {
            Self {
                choice: None,
                calls: Cell::new(0),
            }
        }
    }
    impl EnvPicker for ScriptedPicker {
        fn pick(&self, candidates: &[EnvironmentListEntry]) -> Result<EnvironmentListEntry> {
            self.calls.set(self.calls.get() + 1);
            match self.choice {
                Some(i) => Ok(candidates[i].clone()),
                None => bail!("not a terminal; pass --env <name>"),
            }
        }
    }

    fn prefs() -> (tempfile::TempDir, FilePreferenceStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilePreferenceStore::new(tmp.path().join("preferences.json"));
        (tmp, store)
    }

    #[tokio::test]
    async fn manifest_single_env_is_used_without_picker_or_persistence() {
        let c = client(vec![entry("prod", "demo"), entry("prod", "other")]);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/work/demo");

        let got = select_environment(&c, Some("demo"), &dir, None, &mut store, &NoPicker)
            .await
            .unwrap();

        assert_eq!(got.name, "prod");
        assert_eq!(got.project, "demo");
        assert!(
            store.get(&dir).is_none(),
            "a single env needs nothing remembered"
        );
    }

    #[tokio::test]
    async fn env_flag_pins_by_name_within_project_and_is_not_persisted() {
        let c = client(vec![entry("prod", "demo"), entry("staging", "demo")]);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/work/demo");

        let got = select_environment(
            &c,
            Some("demo"),
            &dir,
            Some("staging"),
            &mut store,
            &NoPicker,
        )
        .await
        .unwrap();

        assert_eq!(got.name, "staging");
        assert!(store.get(&dir).is_none(), "--env is an ephemeral override");
    }

    #[tokio::test]
    async fn env_flag_unknown_name_errors_and_never_creates() {
        let c = client(vec![entry("prod", "demo")]);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/work/demo");

        let err = select_environment(&c, Some("demo"), &dir, Some("ghost"), &mut store, &NoPicker)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ghost"), "names the missing env: {msg}");
        assert!(msg.contains("demo"), "names the project: {msg}");
    }

    #[tokio::test]
    async fn multiple_without_pref_opens_picker_and_remembers_pick() {
        let envs = vec![entry("prod", "demo"), entry("staging", "demo")];
        let staging_id = envs[1].id;
        let c = client(envs);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/work/demo");
        let picker = ScriptedPicker::picks(1);

        let got = select_environment(&c, Some("demo"), &dir, None, &mut store, &picker)
            .await
            .unwrap();

        assert_eq!(got.name, "staging");
        assert_eq!(picker.calls.get(), 1);
        assert_eq!(
            store.get(&dir).unwrap().env_id,
            staging_id,
            "the pick should be remembered for next time"
        );
    }

    #[tokio::test]
    async fn valid_remembered_choice_skips_the_picker() {
        let envs = vec![entry("prod", "demo"), entry("staging", "demo")];
        let prod_id = envs[0].id;
        let c = client(envs);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/work/demo");
        store
            .set(
                &dir,
                EnvRef {
                    env_id: prod_id,
                    env_name: "prod".into(),
                    project: "demo".into(),
                },
            )
            .unwrap();

        let got = select_environment(&c, Some("demo"), &dir, None, &mut store, &NoPicker)
            .await
            .unwrap();

        assert_eq!(got.id, prod_id);
    }

    #[tokio::test]
    async fn stale_remembered_choice_reprompts_and_updates() {
        let envs = vec![entry("prod", "demo"), entry("staging", "demo")];
        let staging_id = envs[1].id;
        let c = client(envs);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/work/demo");
        // A remembered env that no longer exists (deleted since last run).
        store
            .set(
                &dir,
                EnvRef {
                    env_id: Uuid::new_v4(),
                    env_name: "gone".into(),
                    project: "demo".into(),
                },
            )
            .unwrap();
        let picker = ScriptedPicker::picks(1);

        let got = select_environment(&c, Some("demo"), &dir, None, &mut store, &picker)
            .await
            .unwrap();

        assert_eq!(got.name, "staging");
        assert_eq!(picker.calls.get(), 1, "stale pref must re-prompt");
        assert_eq!(
            store.get(&dir).unwrap().env_id,
            staging_id,
            "and update the memory"
        );
    }

    #[tokio::test]
    async fn no_environments_for_project_errors_pointing_at_up() {
        let c = client(vec![entry("prod", "other")]);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/work/demo");

        let err = select_environment(&c, Some("demo"), &dir, None, &mut store, &NoPicker)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("up"), "{err:#}");
    }

    #[tokio::test]
    async fn no_manifest_chooses_across_all_environments() {
        // With no project context, every environment is a candidate.
        let envs = vec![
            entry("prod", "demo"),
            entry("prod", "other"),
            entry("dev", "third"),
        ];
        let c = client(envs);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/tmp/somewhere");
        let picker = ScriptedPicker::picks(2);

        let got = select_environment(&c, None, &dir, None, &mut store, &picker)
            .await
            .unwrap();

        assert_eq!(got.project, "third");
        assert_eq!(picker.calls.get(), 1);
    }

    #[tokio::test]
    async fn no_manifest_single_environment_is_used_directly() {
        let c = client(vec![entry("prod", "demo")]);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/tmp/somewhere");

        let got = select_environment(&c, None, &dir, None, &mut store, &NoPicker)
            .await
            .unwrap();
        assert_eq!(got.name, "prod");
    }

    #[tokio::test]
    async fn no_manifest_env_flag_unique_across_projects_resolves() {
        let c = client(vec![entry("prod", "demo"), entry("staging", "other")]);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/tmp/somewhere");

        let got = select_environment(&c, None, &dir, Some("staging"), &mut store, &NoPicker)
            .await
            .unwrap();
        assert_eq!(got.project, "other");
    }

    #[tokio::test]
    async fn no_manifest_env_flag_ambiguous_across_projects_opens_picker() {
        let envs = vec![entry("prod", "demo"), entry("prod", "other")];
        let other_id = envs[1].id;
        let c = client(envs);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/tmp/somewhere");
        let picker = ScriptedPicker::picks(1);

        let got = select_environment(&c, None, &dir, Some("prod"), &mut store, &picker)
            .await
            .unwrap();

        assert_eq!(got.id, other_id);
        assert_eq!(picker.calls.get(), 1);
        assert!(
            store.get(&dir).is_none(),
            "--env-driven picks stay ephemeral"
        );
    }

    #[tokio::test]
    async fn picker_unavailable_propagates_error() {
        let c = client(vec![entry("prod", "demo"), entry("staging", "demo")]);
        let (_tmp, mut store) = prefs();
        let dir = PathBuf::from("/work/demo");
        let picker = ScriptedPicker::unavailable();

        let err = select_environment(&c, Some("demo"), &dir, None, &mut store, &picker)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("--env"), "{err:#}");
    }
}
