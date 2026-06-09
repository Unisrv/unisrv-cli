//! Locate the project manifest (`unisrv.hcl`) for a command.
//!
//! `up`/`destroy`/`instance` all operate on the project defined by the manifest
//! in the directory they're run from. Resolution is deliberately scoped to that
//! single directory and does **not** climb into parent directories: running from
//! an unrelated subdirectory should never silently adopt a parent project's
//! manifest (especially dangerous for an irreversible `destroy`). The starting
//! directory is a parameter (not read from the process) so lookup is a pure
//! function of the filesystem and testable with tempdirs.

use std::path::{Path, PathBuf};

/// The manifest filename every command resolves against.
pub const CONFIG_FILE: &str = "unisrv.hcl";

/// A located manifest: the directory that holds it (the project root from the
/// CLI's perspective, and the key under which an env choice is remembered) and
/// the full path to the file itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestLocation {
    pub dir: PathBuf,
    pub path: PathBuf,
}

/// Look for `filename` directly in `start`. Returns `None` when it isn't there.
/// `is_file` (not `exists`) so a directory named `unisrv.hcl` doesn't masquerade
/// as a manifest.
pub fn find_config(start: &Path, filename: &str) -> Option<ManifestLocation> {
    let path = start.join(filename);
    if path.is_file() {
        Some(ManifestLocation {
            dir: start.to_path_buf(),
            path,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_manifest_in_start_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(CONFIG_FILE), "project = \"x\"\n").unwrap();

        let found = find_config(tmp.path(), CONFIG_FILE).expect("manifest should be found");
        assert_eq!(found.dir, tmp.path());
        assert_eq!(found.path, tmp.path().join(CONFIG_FILE));
    }

    #[test]
    fn does_not_climb_into_parent_directories() {
        // A manifest in an ancestor must NOT be adopted when running from a
        // subdirectory: lookup is scoped to the literal start directory.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(CONFIG_FILE), "project = \"x\"\n").unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(
            find_config(&nested, CONFIG_FILE).is_none(),
            "a parent's manifest must not be picked up from a subdirectory"
        );
    }

    #[test]
    fn returns_none_when_no_manifest_present() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_config(tmp.path(), CONFIG_FILE).is_none());
    }
}
