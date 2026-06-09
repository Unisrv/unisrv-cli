//! Interpolation variables supplied on the command line.
//!
//! Values come from `--var KEY=VALUE` flags and `--var-file` dotenv files, are
//! always strings, and are merged into a single map. Any key set more than once
//! across all sources is an error (there is no override precedence).

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::config::{UpConfig, VarResolution};
use super::env_resolve::Prompter;

/// Interpolation variables, keyed by name. Values are always strings.
pub type VarMap = BTreeMap<String, String>;

/// Resolve the config at `path`, interpolating variables from `base` and
/// filling any that are missing.
///
/// When `interactive`, missing variables are prompted for (via `prompter`) and
/// resolution is retried until complete. Otherwise a missing variable is a hard
/// error naming the unset variables — the right behaviour for non-TTY runs (CI,
/// piped input) where there is no one to prompt.
pub fn resolve_config(
    path: &Path,
    source: &str,
    base: VarMap,
    interactive: bool,
    prompter: &dyn Prompter,
) -> Result<UpConfig> {
    let mut vars = base;
    let mut prev_missing: Option<BTreeSet<String>> = None;
    loop {
        match UpConfig::resolve(path, source, &vars)? {
            VarResolution::Resolved(cfg) => return Ok(cfg),
            VarResolution::Missing(missing) => {
                // If prompting didn't shrink the missing set, the references
                // can't be satisfied by supplying variables (e.g. an object-key
                // miss that isn't really a `var`). Bail rather than loop forever.
                if prev_missing.as_ref() == Some(&missing) {
                    let names: Vec<&str> = missing.iter().map(String::as_str).collect();
                    bail!("could not resolve references: {}", names.join(", "));
                }
                if !interactive {
                    let names: Vec<&str> = missing.iter().map(String::as_str).collect();
                    bail!(
                        "missing values for variable(s): {}. Supply them with --var KEY=VALUE \
                         or --var-file <file> (no interactive terminal available to prompt).",
                        names.join(", ")
                    );
                }
                for name in &missing {
                    let value = prompter.prompt_string(&format!("Value for var.{name}"), None)?;
                    vars.insert(name.clone(), value);
                }
                prev_missing = Some(missing);
            }
        }
    }
}

/// Merge `--var` flag assignments and dotenv file contents into a single map.
///
/// `files` is `(label, contents)` where `label` is shown in error messages.
/// Every key must be set exactly once across all sources combined — there is no
/// override precedence, so any duplicate (within a file or across sources) is an
/// error.
pub fn collect(flags: &[String], files: &[(String, String)]) -> Result<VarMap> {
    // Track each key's value and the source it came from, so a collision can
    // name both sources.
    let mut entries: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut insert = |key: String, value: String, source: String| -> Result<()> {
        if let Some((_, first)) = entries.get(&key) {
            bail!(
                "variable {key:?} is set more than once (in {first} and {source}); \
                 each variable may be set only once"
            );
        }
        entries.insert(key, (value, source));
        Ok(())
    };
    for (label, contents) in files {
        let pairs = parse_var_file(contents).with_context(|| format!("failed to parse {label}"))?;
        for (key, value) in pairs {
            insert(key, value, format!("--var-file {label}"))?;
        }
    }
    for flag in flags {
        let (key, value) = parse_assignment(flag)?;
        insert(key, value, "a --var flag".to_string())?;
    }
    Ok(entries.into_iter().map(|(k, (v, _))| (k, v)).collect())
}

/// Parse one `KEY=VALUE` assignment. The key and value are trimmed; the value
/// is split off at the first `=` so it may itself contain `=`.
pub fn parse_assignment(s: &str) -> Result<(String, String)> {
    let Some((key, value)) = s.split_once('=') else {
        bail!("invalid variable assignment {s:?}: expected KEY=VALUE");
    };
    let key = key.trim();
    validate_key(key)?;
    Ok((key.to_string(), value.trim().to_string()))
}

/// Parse dotenv-style file contents into ordered `(key, value)` pairs. Blank
/// lines and `#`-prefixed comment lines are skipped; every other line is parsed
/// as a `KEY=VALUE` assignment. Duplicate keys are *not* rejected here — that's
/// the job of [`collect`], which sees every source at once.
pub fn parse_var_file(contents: &str) -> Result<Vec<(String, String)>> {
    // Strip a leading UTF-8 BOM (common from Windows editors / PowerShell) so it
    // doesn't end up glued to the first key.
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let mut pairs = Vec::new();
    for (i, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, value) =
            parse_assignment(trimmed).with_context(|| format!("on line {}", i + 1))?;
        pairs.push((key, value));
    }
    Ok(pairs)
}

/// A variable name is referenced as `var.<key>`, so it must be a valid
/// identifier: a leading letter or underscore followed by letters, digits, or
/// underscores.
fn validate_key(key: &str) -> Result<()> {
    let valid = {
        let mut chars = key.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            }
            _ => false,
        }
    };
    if !valid {
        bail!(
            "invalid variable name {key:?}: must start with a letter or underscore \
             and contain only letters, digits, or underscores"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Returns scripted answers keyed by the variable name embedded in the
    /// prompt ("Value for var.<name>"). Panics if asked for an unscripted name.
    struct ScriptedPrompter {
        answers: BTreeMap<String, String>,
        asked: RefCell<Vec<String>>,
    }

    impl ScriptedPrompter {
        fn new(answers: &[(&str, &str)]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl Prompter for ScriptedPrompter {
        fn prompt_string(&self, prompt: &str, _default: Option<&str>) -> Result<String> {
            let name = prompt.strip_prefix("Value for var.").unwrap_or(prompt);
            self.asked.borrow_mut().push(name.to_string());
            // Turn a runaway resolve loop into a clean test failure rather than a
            // hang, so a missing/broken termination guard is caught.
            assert!(
                self.asked.borrow().len() <= 50,
                "prompted too many times — resolve loop is not terminating"
            );
            Ok(self
                .answers
                .get(name)
                .unwrap_or_else(|| panic!("no scripted answer for {name:?}"))
                .clone())
        }
        fn prompt_optional(&self, _prompt: &str) -> Result<Option<String>> {
            unimplemented!("variable prompting never asks for optional values")
        }
    }

    const ONE_VAR_SRC: &str = r#"
project = "demo"
deployment "app" {
  container {
    image = "myapp:${var.tag}"
  }
}
"#;

    #[test]
    fn resolve_config_prompts_for_missing_var_when_interactive() {
        let prompter = ScriptedPrompter::new(&[("tag", "v9")]);
        let cfg = resolve_config(
            Path::new("unisrv.hcl"),
            ONE_VAR_SRC,
            VarMap::new(),
            true,
            &prompter,
        )
        .unwrap();
        assert_eq!(cfg.deployment["app"].container.image, "myapp:v9");
        assert_eq!(*prompter.asked.borrow(), vec!["tag".to_string()]);
    }

    #[test]
    fn parse_assignment_splits_key_and_value() {
        assert_eq!(
            parse_assignment("image_tag=v1.2.3").unwrap(),
            ("image_tag".to_string(), "v1.2.3".to_string())
        );
    }

    #[test]
    fn parse_assignment_value_may_contain_equals_or_be_empty() {
        assert_eq!(
            parse_assignment("dsn=postgres://u:p@h/db?x=1").unwrap(),
            ("dsn".to_string(), "postgres://u:p@h/db?x=1".to_string())
        );
        assert_eq!(
            parse_assignment("empty=").unwrap(),
            ("empty".to_string(), String::new())
        );
    }

    #[test]
    fn resolve_config_bails_when_missing_set_cannot_shrink() {
        // An object-literal attribute miss yields a NoSuchKey that looks like a
        // missing var but can never be satisfied by prompting. The loop must
        // bail when the missing set stops shrinking, not spin forever.
        let src = r#"
project = "demo"
deployment "app" {
  container {
    image = {a = "x"}.b
  }
}
"#;
        let prompter = ScriptedPrompter::new(&[("b", "whatever")]);
        let err = resolve_config(Path::new("unisrv.hcl"), src, VarMap::new(), true, &prompter)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains('b'),
            "should report the unresolved reference"
        );
    }

    #[test]
    fn resolve_config_errors_when_not_interactive_and_var_missing() {
        let prompter = ScriptedPrompter::new(&[]);
        let err = resolve_config(
            Path::new("unisrv.hcl"),
            ONE_VAR_SRC,
            VarMap::new(),
            false,
            &prompter,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("tag"), "should name the missing var: {msg}");
        assert!(msg.contains("--var"), "should suggest the flags: {msg}");
        assert!(
            prompter.asked.borrow().is_empty(),
            "must not prompt in non-interactive mode"
        );
    }

    #[test]
    fn resolve_config_prompts_for_each_missing_var() {
        // Two missing vars → both prompted, resolution completes.
        let src = r#"
project = "demo"
deployment "app" {
  container {
    image = "myapp:${var.tag}"
    env = {
      API_URL = var.api_url
    }
  }
}
"#;
        let prompter = ScriptedPrompter::new(&[("tag", "v1"), ("api_url", "https://x")]);
        let cfg =
            resolve_config(Path::new("unisrv.hcl"), src, VarMap::new(), true, &prompter).unwrap();
        assert_eq!(cfg.deployment["app"].container.image, "myapp:v1");
        assert_eq!(
            cfg.deployment["app"].container.env.as_ref().unwrap()["API_URL"],
            "https://x"
        );
    }

    #[test]
    fn collect_merges_distinct_keys_from_flags_and_files() {
        let flags = ["tag=v1".to_string()];
        let files = [("prod.vars".to_string(), "api_url=https://x\n".to_string())];
        let map = collect(&flags, &files).unwrap();
        assert_eq!(map["tag"], "v1");
        assert_eq!(map["api_url"], "https://x");
    }

    #[test]
    fn collect_rejects_duplicate_key_across_sources() {
        // No override precedence: the same key in a file and a flag is an error.
        let flags = ["tag=v2".to_string()];
        let files = [("base.vars".to_string(), "tag=v1\n".to_string())];
        let err = collect(&flags, &files).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("tag"), "should name the key: {msg}");
        assert!(msg.contains("more than once"), "msg: {msg}");
    }

    #[test]
    fn collect_duplicate_error_names_the_sources() {
        let flags = ["tag=v2".to_string()];
        let files = [("base.vars".to_string(), "tag=v1\n".to_string())];
        let err = collect(&flags, &files).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("base.vars"),
            "should name the file source: {msg}"
        );
        assert!(msg.contains("--var"), "should name the flag source: {msg}");
    }

    #[test]
    fn collect_rejects_duplicate_key_within_a_file() {
        let files = [("dup.vars".to_string(), "k=1\nk=2\n".to_string())];
        let err = collect(&[], &files).unwrap_err();
        assert!(format!("{err:#}").contains("k"), "should name the key");
    }

    #[test]
    fn parse_var_file_strips_leading_bom() {
        // Files saved on Windows / via PowerShell redirection often start with a
        // UTF-8 BOM. It must not become part of the first key.
        let contents = "\u{feff}tag=v1\napi=https://x\n";
        assert_eq!(
            parse_var_file(contents).unwrap(),
            vec![
                ("tag".to_string(), "v1".to_string()),
                ("api".to_string(), "https://x".to_string()),
            ]
        );
    }

    #[test]
    fn parse_var_file_skips_comments_and_blanks_and_trims() {
        let contents = "\
# prod overrides
image_tag=v1.2.3

api_url = https://api.example.com
";
        assert_eq!(
            parse_var_file(contents).unwrap(),
            vec![
                ("image_tag".to_string(), "v1.2.3".to_string()),
                ("api_url".to_string(), "https://api.example.com".to_string()),
            ]
        );
    }

    #[test]
    fn parse_assignment_rejects_invalid_key() {
        // Keys must be valid identifiers: they become `var.<key>` references.
        for bad in [
            "1leading=x",
            "with.dot=x",
            "with-dash=x",
            "with space=x",
            "=x",
        ] {
            let err = parse_assignment(bad).unwrap_err();
            assert!(
                format!("{err:#}").contains("variable name"),
                "{bad:?} should be rejected as an invalid variable name"
            );
        }
    }
}
