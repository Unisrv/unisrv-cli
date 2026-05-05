//! Typed view of `unisrv.hcl`.
//!
//! Parsing is done with `hcl-rs` via serde derive into typed structs. Variable
//! interpolation is *not* supported yet — when we add it, the migration path is
//! to parse to `hcl::Body`, evaluate, then deserialize into these same types.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpConfig {
    pub project: String,
    #[serde(default)]
    pub service: BTreeMap<String, ServiceBlock>,
    #[serde(default)]
    pub deployment: BTreeMap<String, DeploymentBlock>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServiceBlock {
    pub host: String,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentBlock {
    /// Name of a `service` block this deployment binds to (optional — bare
    /// deployments without a service-fronting are valid).
    #[serde(default)]
    pub service: Option<String>,
    /// Port that the container listens on. Required when `service` is set.
    #[serde(default)]
    pub port: Option<u16>,
    pub container: ContainerBlock,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContainerBlock {
    pub image: String,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
}

impl UpConfig {
    pub fn parse(source: &str) -> Result<Self> {
        // Parse to a structural Body first so we can catch issues that
        // `hcl-rs`'s serde path silently swallows: duplicate labeled blocks
        // (it merges them into a malformed expression value) and empty labels.
        let body: hcl::Body = hcl::from_str(source).context("failed to parse unisrv.hcl")?;
        validate_blocks(&body)?;
        let cfg: Self = hcl::from_body(body).context("failed to parse unisrv.hcl")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&source)
    }

    fn validate(&self) -> Result<()> {
        if self.project.trim().is_empty() {
            bail!("`project` must be a non-empty string");
        }
        for (name, dep) in &self.deployment {
            if let Some(svc) = &dep.service {
                if !self.service.contains_key(svc) {
                    bail!(
                        "deployment \"{name}\" references service \"{svc}\" which is not defined"
                    );
                }
                if dep.port.is_none() {
                    bail!(
                        "deployment \"{name}\" binds to service \"{svc}\" but has no `port` set"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Walks the parsed body, rejecting empty labels and duplicate labeled blocks
/// at any nesting depth. `hcl-rs` accepts both, but the deserializer either
/// silently merges duplicates into a malformed expression value or yields a
/// blank-keyed entry that survives all the way to API calls.
fn validate_blocks(body: &hcl::Body) -> Result<()> {
    let mut seen: BTreeSet<(String, Vec<String>)> = BTreeSet::new();
    for block in body.blocks() {
        let kind = block.identifier();
        let labels: Vec<&str> = block.labels().iter().map(|l| l.as_str()).collect();

        for label in &labels {
            if label.is_empty() {
                bail!("`{kind}` block has an empty label; labels must be non-empty");
            }
        }

        if !labels.is_empty() {
            let key = (
                kind.to_string(),
                labels.iter().map(|s| s.to_string()).collect(),
            );
            if !seen.insert(key) {
                bail!(
                    "duplicate `{kind} \"{}\"` block",
                    labels.join("\" \"")
                );
            }
        }

        validate_blocks(block.body())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_nginx_example() {
        let src = r#"
project = "nginx-demo"

service "nginx" {
  host = "nginx.unisrv.dev"
}

deployment "nginx" {
  service = "nginx"
  port    = 80
  container {
    image = "nginx"
  }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        assert_eq!(cfg.project, "nginx-demo");
        assert_eq!(cfg.service.len(), 1);
        assert_eq!(cfg.service["nginx"].host, "nginx.unisrv.dev");

        let dep = &cfg.deployment["nginx"];
        assert_eq!(dep.service.as_deref(), Some("nginx"));
        assert_eq!(dep.port, Some(80));
        assert_eq!(dep.container.image, "nginx");
        assert!(dep.container.args.is_none());
        assert!(dep.container.env.is_none());
    }

    #[test]
    fn parses_container_args_and_env() {
        let src = r#"
project = "demo"

deployment "app" {
  container {
    image = "myapp:1.0"
    args  = ["--config", "/etc/app.conf"]
    env = {
      LOG_LEVEL    = "info"
      DATABASE_URL = "postgres://db/app"
    }
  }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        let dep = &cfg.deployment["app"];
        assert_eq!(
            dep.container.args.as_ref().map(|v| v.as_slice()),
            Some(
                [String::from("--config"), String::from("/etc/app.conf")].as_slice(),
            ),
        );
        let env = dep.container.env.as_ref().unwrap();
        assert_eq!(env["LOG_LEVEL"], "info");
        assert_eq!(env["DATABASE_URL"], "postgres://db/app");
    }

    #[test]
    fn rejects_unknown_top_level_attr() {
        let src = r#"
project = "x"
unknown_field = 5
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown_field"), "error was: {msg}");
    }

    #[test]
    fn rejects_unknown_container_attr() {
        let src = r#"
project = "x"
deployment "d" {
  container {
    image = "x"
    typo  = "oops"
  }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("typo"), "error was: {msg}");
    }

    #[test]
    fn rejects_deployment_referencing_undefined_service() {
        let src = r#"
project = "x"
deployment "d" {
  service = "missing"
  port = 80
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("missing"));
    }

    #[test]
    fn rejects_service_bound_deployment_without_port() {
        let src = r#"
project = "x"
service "s" { host = "h.example" }
deployment "d" {
  service = "s"
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("port"));
    }

    #[test]
    fn rejects_empty_project() {
        let src = r#"project = """#;
        let err = UpConfig::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("project"));
    }

    #[test]
    fn rejects_duplicate_service_blocks() {
        let src = r#"
project = "x"
service "web" { host = "a.example" }
service "web" { host = "b.example" }
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("duplicate"), "error was: {msg}");
        assert!(msg.contains("service"), "error was: {msg}");
        assert!(msg.contains("web"), "error was: {msg}");
    }

    #[test]
    fn rejects_duplicate_deployment_blocks() {
        let src = r#"
project = "x"
deployment "app" {
  container { image = "i:1" }
}
deployment "app" {
  container { image = "i:2" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("duplicate"), "error was: {msg}");
        assert!(msg.contains("deployment"), "error was: {msg}");
        assert!(msg.contains("app"), "error was: {msg}");
    }

    #[test]
    fn rejects_empty_service_label() {
        let src = r#"
project = "x"
service "" { host = "a.example" }
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("empty label"), "error was: {msg}");
        assert!(msg.contains("service"), "error was: {msg}");
    }

    #[test]
    fn rejects_empty_deployment_label() {
        let src = r#"
project = "x"
deployment "" {
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("empty label"), "error was: {msg}");
        assert!(msg.contains("deployment"), "error was: {msg}");
    }

    #[test]
    fn parses_bare_deployment_without_service() {
        let src = r#"
project = "x"
deployment "worker" {
  container { image = "worker:1" }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        let dep = &cfg.deployment["worker"];
        assert!(dep.service.is_none());
        assert!(dep.port.is_none());
    }
}
