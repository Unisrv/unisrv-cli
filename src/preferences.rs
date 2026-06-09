//! Remembered per-directory environment selections.
//!
//! When a project has several environments and the user doesn't pin one with
//! `--env`, the CLI prompts once and remembers the choice so later commands in
//! the same directory don't re-prompt. Choices are keyed by directory (the
//! manifest's directory, or the literal CWD when there is no manifest) and
//! persisted to `~/.unisrv/preferences.json` next to the auth store.
//!
//! Preferences are strictly best-effort UX state: a missing or corrupt file is
//! treated as "nothing remembered", never an error — losing a remembered pick
//! only costs one extra prompt.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A remembered environment choice. The id is authoritative (used to
/// revalidate against the live environment list); name and project are kept for
/// display and so a human can read the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvRef {
    pub env_id: Uuid,
    pub env_name: String,
    pub project: String,
}

/// Read/remember a directory's chosen environment.
pub trait PreferenceStore {
    /// The environment remembered for `dir`, if any.
    fn get(&self, dir: &Path) -> Option<EnvRef>;
    /// Remember `env` as the choice for `dir`, persisting it.
    fn set(&mut self, dir: &Path, env: EnvRef) -> Result<()>;
}

/// On-disk document: directory path → chosen environment.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PreferencesDoc {
    #[serde(default)]
    environments: BTreeMap<String, EnvRef>,
}

/// JSON-file-backed [`PreferenceStore`] at a fixed path.
pub struct FilePreferenceStore {
    path: PathBuf,
}

impl FilePreferenceStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The default location, `~/.unisrv/preferences.json` (next to the auth
    /// store). `None` if the home directory can't be determined.
    pub fn default_path() -> Option<PathBuf> {
        Some(unisrv_api::config_dir()?.join("preferences.json"))
    }

    /// Load the document, treating a missing or unparseable file as empty.
    fn load(&self) -> PreferencesDoc {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

/// The map key for a directory. Path strings are used verbatim so the file is
/// human-readable; callers pass absolute directories.
fn key(dir: &Path) -> String {
    dir.to_string_lossy().into_owned()
}

/// A [`PreferenceStore`] that remembers nothing. Used when there's no home
/// directory to anchor to: rather than scatter state into a shared temp file
/// (which would bleed one invocation's pick into another), the CLI simply
/// re-prompts each time — exactly the documented best-effort fallback.
pub struct NullPreferenceStore;

impl PreferenceStore for NullPreferenceStore {
    fn get(&self, _dir: &Path) -> Option<EnvRef> {
        None
    }
    fn set(&mut self, _dir: &Path, _env: EnvRef) -> Result<()> {
        Ok(())
    }
}

impl PreferenceStore for FilePreferenceStore {
    fn get(&self, dir: &Path) -> Option<EnvRef> {
        self.load().environments.get(&key(dir)).cloned()
    }

    fn set(&mut self, dir: &Path, env: EnvRef) -> Result<()> {
        let mut doc = self.load();
        doc.environments.insert(key(dir), env);

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("failed to write {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_ref(name: &str) -> EnvRef {
        EnvRef {
            env_id: Uuid::new_v4(),
            env_name: name.to_string(),
            project: "demo".to_string(),
        }
    }

    fn store_at(tmp: &tempfile::TempDir) -> FilePreferenceStore {
        FilePreferenceStore::new(tmp.path().join("preferences.json"))
    }

    #[test]
    fn set_then_get_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(&tmp);
        let dir = Path::new("/home/dev/project");
        let chosen = env_ref("prod");

        store.set(dir, chosen.clone()).unwrap();

        assert_eq!(store.get(dir), Some(chosen));
    }

    #[test]
    fn choices_are_independent_per_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(&tmp);
        let a = Path::new("/work/project-a");
        let b = Path::new("/work/project-b");

        store.set(a, env_ref("prod")).unwrap();
        store.set(b, env_ref("staging")).unwrap();

        assert_eq!(store.get(a).unwrap().env_name, "prod");
        assert_eq!(store.get(b).unwrap().env_name, "staging");
    }

    #[test]
    fn unknown_directory_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(&tmp);
        assert!(store.get(Path::new("/never/chosen")).is_none());
    }

    #[test]
    fn missing_file_is_treated_as_empty() {
        // A fresh install has no preferences file at all; reading it must be a
        // clean "nothing remembered", not an error.
        let store = FilePreferenceStore::new(PathBuf::from("/no/such/preferences.json"));
        assert!(store.get(Path::new("/anything")).is_none());
    }

    #[test]
    fn corrupt_file_is_treated_as_empty() {
        // A hand-mangled or partially-written file must degrade to "nothing
        // remembered" rather than breaking every command that reads it.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("preferences.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let store = FilePreferenceStore::new(path);
        assert!(store.get(Path::new("/anything")).is_none());
    }

    #[test]
    fn set_overwrites_previous_choice_for_same_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(&tmp);
        let dir = Path::new("/work/project");

        store.set(dir, env_ref("prod")).unwrap();
        store.set(dir, env_ref("staging")).unwrap();

        assert_eq!(store.get(dir).unwrap().env_name, "staging");
    }
}
