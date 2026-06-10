//! Typed view of `unisrv.hcl`.
//!
//! Parsing goes through `hcl-rs`: source is parsed to a structural `hcl::Body`,
//! `${var.X}` references are evaluated against caller-supplied variables, and
//! the result is deserialized into these typed structs via serde. See
//! [`UpConfig::resolve`]; command-line variable handling lives in [`super::vars`].

use anyhow::{Context, Result};
use hcl::eval::Evaluate;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::parse_error::{ConfigParseError, Locator};

/// Outcome of resolving a config against a set of interpolation variables.
///
/// Resolution is deliberately *not* all-or-nothing: a config that references
/// `${var.X}` for which no value was supplied comes back as [`Missing`] listing
/// the names, so the caller can prompt for them and try again — rather than
/// failing outright.
///
/// [`Missing`]: VarResolution::Missing
#[derive(Debug)]
pub enum VarResolution {
    /// Every referenced variable had a value; the typed config is ready.
    Resolved(UpConfig),
    /// These variable names are referenced but were not supplied.
    Missing(BTreeSet<String>),
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpConfig {
    pub project: String,
    #[serde(default)]
    pub service: BTreeMap<String, ServiceBlock>,
    #[serde(default)]
    pub deployment: BTreeMap<String, DeploymentBlock>,
    #[serde(default)]
    pub network: BTreeMap<String, NetworkBlock>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NetworkBlock {
    /// IPv4 CIDR block for the network (e.g. "10.0.0.0/16"). Optional —
    /// defaults to [`super::defaults::DEFAULT_NETWORK_CIDR`] downstream.
    #[serde(default)]
    pub iprange: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServiceBlock {
    /// Custom hosts to bind to this service. Optional — every service is always
    /// reachable at its derived base host (`{name}-{env-slug}.unisrv.dev`)
    /// regardless of this list.
    #[serde(default)]
    pub hosts: Option<Vec<String>>,
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
    /// Name of a `network` block whose network all instances join (optional).
    /// The referenced network must be defined in this file.
    #[serde(default)]
    pub network: Option<String>,
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
    /// Parse with no interpolation variables, expecting none to be referenced.
    /// Test convenience over [`resolve`](Self::resolve).
    #[cfg(test)]
    pub fn parse(source: &str) -> Result<Self> {
        match Self::resolve(Path::new("unisrv.hcl"), source, &BTreeMap::new())? {
            VarResolution::Resolved(cfg) => Ok(cfg),
            VarResolution::Missing(missing) => {
                anyhow::bail!("config references unset variables: {missing:?}")
            }
        }
    }

    /// Parse `source`, interpolating `${var.X}` references from `vars`.
    ///
    /// Structural validation (labels, duplicate blocks) happens on the raw body
    /// first; the body is then evaluated with `vars` bound under a `var` object
    /// and deserialized into the typed config, which is finally validated with
    /// concrete (substituted) values.
    pub fn resolve(
        path: &Path,
        source: &str,
        vars: &BTreeMap<String, String>,
    ) -> Result<VarResolution> {
        let body = parse_body(path, source)?;

        let mut ctx = hcl::eval::Context::new();
        ctx.declare_var("var", var_object(vars));

        let mut evaluated = body.clone();
        if let Err(errors) = evaluated.evaluate_in_place(&ctx) {
            // A reference to an absent key under the `var` object surfaces as
            // `NoSuchKey`; we report those as missing interpolation variables so
            // the caller can prompt for them. A `NoSuchKey` can also come from a
            // user-side object access that isn't a `var` — that gets reported as
            // missing too, but the resolve loop bails when prompting fails to
            // shrink the set. Any other eval error (e.g. a typo'd namespace
            // yielding an undefined variable) is a real fault, surfaced now.
            let mut missing = BTreeSet::new();
            for err in &errors {
                match err.kind() {
                    hcl::eval::ErrorKind::NoSuchKey(key) => {
                        missing.insert(key.clone());
                    }
                    _ => {
                        // Eval errors carry no source span; point the caret at
                        // the offending identifier (best-effort) so the report
                        // looks like the rest of our config errors.
                        let locator = match err.kind() {
                            hcl::eval::ErrorKind::UndefinedVar(ident) => {
                                Some(Locator::substring(ident.as_str()))
                            }
                            _ => None,
                        };
                        return Err(ConfigParseError::validation(
                            path,
                            source,
                            err.to_string(),
                            locator,
                        )
                        .into());
                    }
                }
            }
            return Ok(VarResolution::Missing(missing));
        }

        let cfg: Self =
            hcl::from_body(evaluated).map_err(|e| ConfigParseError::from_hcl(path, source, e))?;
        cfg.validate(path, source)?;
        Ok(VarResolution::Resolved(cfg))
    }

    /// Read just the top-level `project` name, without interpolation.
    ///
    /// `destroy` needs only the project and supplies no variables, so it must
    /// not evaluate (or deserialize) the rest of the body — which may reference
    /// unprovided `${var.…}`. `project` is required to be a literal, so it can
    /// be read directly off the raw body.
    pub fn parse_project_at(path: &Path, source: &str) -> Result<String> {
        let body = parse_body(path, source)?;
        for attr in body.attributes() {
            if attr.key() == "project"
                && let hcl::Expression::String(s) = attr.expr()
            {
                if s.trim().is_empty() {
                    return Err(ConfigParseError::validation(
                        path,
                        source,
                        "`project` must be a non-empty string",
                        Some(Locator::field("project")),
                    )
                    .into());
                }
                return Ok(s.clone());
            }
        }
        Err(ConfigParseError::validation(path, source, "missing field `project`", None).into())
    }

    /// Read just the `project` name from the config file at `path`.
    pub fn load_project(path: &Path) -> Result<String> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse_project_at(path, &source)
    }

    fn validate(&self, path: &Path, source: &str) -> Result<(), ConfigParseError> {
        let err = |msg, loc| ConfigParseError::validation(path, source, msg, loc);
        if self.project.trim().is_empty() {
            return Err(err(
                "`project` must be a non-empty string".into(),
                Some(Locator::field("project")),
            ));
        }
        let mut seen_hosts: BTreeMap<String, &str> = BTreeMap::new();
        for (svc_name, svc) in &self.service {
            for host in svc.hosts.iter().flatten() {
                if let Some(reason) = invalid_base_domain_host(host) {
                    return Err(err(
                        reason,
                        Some(Locator::substring(&format!("\"{host}\""))),
                    ));
                }
                // A host binds to exactly one service; the same host under two
                // services would 409 at link time, so reject it up front.
                let key = host.trim_end_matches('.').to_ascii_lowercase();
                if let Some(first) = seen_hosts.insert(key, svc_name) {
                    return Err(err(
                        format!(
                            "host {host:?} is bound to multiple services ({first:?} and {svc_name:?}); a host can belong to only one service"
                        ),
                        Some(Locator::substring(&format!("\"{host}\"")).nth(1)),
                    ));
                }
            }
        }
        for net in self.network.values() {
            if let Some(iprange) = &net.iprange
                && let Some(reason) = invalid_ipv4_cidr(iprange)
            {
                return Err(err(
                    reason,
                    Some(Locator::substring(&format!("\"{iprange}\""))),
                ));
            }
        }
        for (name, dep) in &self.deployment {
            if let Some(svc) = &dep.service {
                if !self.service.contains_key(svc) {
                    return Err(err(
                        format!(
                            "deployment \"{name}\" references service \"{svc}\" which is not defined"
                        ),
                        Some(Locator::substring(&format!("\"{svc}\""))),
                    ));
                }
                if dep.port.is_none() {
                    return Err(err(
                        format!(
                            "deployment \"{name}\" binds to service \"{svc}\" but has no `port` set"
                        ),
                        Some(Locator::substring(&format!("deployment \"{name}\""))),
                    ));
                }
            }
            if let Some(net) = &dep.network
                && !self.network.contains_key(net)
            {
                return Err(err(
                    format!(
                        "deployment \"{name}\" references network \"{net}\" which is not defined"
                    ),
                    Some(Locator::substring(&format!("\"{net}\""))),
                ));
            }
        }
        Ok(())
    }
}

/// Parse `source` to a structural body and run the variable-independent
/// validation shared by both `up` and `destroy`: structural block checks and
/// the literal-`project` rule. Variable evaluation, typed deserialization, and
/// semantic validation happen only on top of this, on the `up` path — so the
/// two commands never diverge on what they consider a structurally valid file.
fn parse_body(path: &Path, source: &str) -> Result<hcl::Body, ConfigParseError> {
    let body: hcl::Body =
        hcl::from_str(source).map_err(|e| ConfigParseError::from_hcl(path, source, e))?;
    validate_blocks(path, source, &body)?;
    reject_interpolated_project(path, source, &body)?;
    Ok(body)
}

/// Reject `${var.…}` interpolation in the top-level `project` attribute.
///
/// `project` is read by `destroy` with no variable context, so it must not
/// depend on interpolation — it has to be a bare string literal. A literal
/// parses as [`hcl::Expression::String`]; anything with a template becomes a
/// `TemplateExpr` (or another expression kind), which we reject here.
fn reject_interpolated_project(
    path: &Path,
    source: &str,
    body: &hcl::Body,
) -> Result<(), ConfigParseError> {
    for attr in body.attributes() {
        if attr.key() == "project" && !matches!(attr.expr(), hcl::Expression::String(_)) {
            return Err(ConfigParseError::validation(
                path,
                source,
                "`project` must be a plain quoted string literal".to_string(),
                Some(Locator::field("project")),
            ));
        }
    }
    Ok(())
}

/// Build the `var` object bound into the evaluation context. Values are always
/// strings (the only var type we support); references like `${var.foo}` resolve
/// to `vars["foo"]`, and references to absent keys surface as eval errors.
fn var_object(vars: &BTreeMap<String, String>) -> hcl::Value {
    hcl::Value::Object(
        vars.iter()
            .map(|(k, v)| (k.clone(), hcl::Value::from(v.clone())))
            .collect(),
    )
}

/// Returns an error message if `iprange` is not a valid IPv4 CIDR block, else
/// `None`. Parses with the same `cidr` crate as the backend, so the CLI and
/// server agree exactly on what's accepted — notably, host bits must be zero
/// (`10.0.0.5/16` is rejected, `10.0.0.0/16` is fine).
fn invalid_ipv4_cidr(iprange: &str) -> Option<String> {
    let err = match iprange.parse::<cidr::Ipv4Cidr>() {
        Ok(_) => return None,
        Err(e) => e,
    };
    // `Ipv4Inet` accepts host bits where `Ipv4Cidr` doesn't; if it parses,
    // the only problem is non-zero host bits, so offer the masked address.
    match iprange.parse::<cidr::Ipv4Inet>() {
        Ok(inet) => {
            let net = inet.network();
            Some(format!(
                "{iprange:?} is not a network address (host bits are set) — did you mean \"{net}\"?"
            ))
        }
        Err(_) => Some(format!(
            "{iprange:?} is not a valid IPv4 CIDR block (e.g. \"10.0.0.0/16\"): {err}"
        )),
    }
}

/// The platform base domain. Custom hosts under it are served by a wildcard
/// certificate and, to avoid colliding with derived base hosts
/// (`{name}-{slug}.unisrv.dev`, which always contain a hyphen), must be a
/// single label of lowercase letters and digits — no hyphens, no subdomains.
const BASE_DOMAIN: &str = "unisrv.dev";

/// Returns an error message if `host` is an invalid custom host under the
/// platform base domain, else `None`. Mirrors the server's base-label rule so
/// the CLI fails fast with a source span instead of waiting for a claim 400.
/// Hosts off the base domain are not checked here (hyphens etc. are fine).
fn invalid_base_domain_host(host: &str) -> Option<String> {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    let label = normalized.strip_suffix(&format!(".{BASE_DOMAIN}"))?;
    let single_label_ok = !label.is_empty()
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if single_label_ok {
        None
    } else {
        Some(format!(
            "host {host:?} is not a valid custom {BASE_DOMAIN} host: it must be a single label of \
             lowercase letters and digits with no hyphens or subdomains (e.g. \"myapp.{BASE_DOMAIN}\")"
        ))
    }
}

/// Walks the parsed body, rejecting empty labels and duplicate labeled blocks
/// at any nesting depth. `hcl-rs` accepts both, but the deserializer either
/// silently merges duplicates into a malformed expression value or yields a
/// blank-keyed entry that survives all the way to API calls.
fn validate_blocks<'a>(
    path: &Path,
    source: &str,
    body: &'a hcl::Body,
) -> Result<(), ConfigParseError> {
    let mut seen: BTreeSet<(&'a str, Vec<&'a str>)> = BTreeSet::new();
    walk_blocks(path, source, body, &mut seen)
}

fn walk_blocks<'a>(
    path: &Path,
    source: &str,
    body: &'a hcl::Body,
    seen: &mut BTreeSet<(&'a str, Vec<&'a str>)>,
) -> Result<(), ConfigParseError> {
    for block in body.blocks() {
        let kind = block.identifier();
        let labels: Vec<&'a str> = block.labels().iter().map(|l| l.as_str()).collect();

        for label in &labels {
            if label.is_empty() {
                return Err(ConfigParseError::validation(
                    path,
                    source,
                    format!("`{kind}` block has an empty label; labels must be non-empty"),
                    Some(Locator::substring(&format!("{kind} \"\""))),
                ));
            }
        }

        if !labels.is_empty() && !seen.insert((kind, labels.clone())) {
            // `seen.insert` returned false → this is the second occurrence,
            // so point the caret at occurrence index 1.
            let needle = format!("{kind} \"{}\"", labels.join("\" \""));
            return Err(ConfigParseError::validation(
                path,
                source,
                format!("duplicate `{kind} \"{}\"` block", labels.join("\" \"")),
                Some(Locator::substring(&needle).nth(1)),
            ));
        }

        walk_blocks(path, source, block.body(), seen)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_with(src: &str, vars: &[(&str, &str)]) -> VarResolution {
        let map: BTreeMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        UpConfig::resolve(Path::new("unisrv.hcl"), src, &map).unwrap()
    }

    #[test]
    fn rejects_interpolation_in_block_label() {
        // hcl does not allow interpolation in block labels — it is a parse
        // error — so a label can never silently ship as a literal `${...}`
        // string. This guards that behaviour against a future hcl change.
        let src = r#"
project = "demo"
service "${var.env}-web" {}
"#;
        assert!(UpConfig::resolve(Path::new("unisrv.hcl"), src, &BTreeMap::new()).is_err());
    }

    #[test]
    fn resolve_substitutes_provided_var_into_template() {
        let src = r#"
project = "demo"
deployment "app" {
  container {
    image = "myapp:${var.image_tag}"
  }
}
"#;
        let cfg = match resolve_with(src, &[("image_tag", "v1.2.3")]) {
            VarResolution::Resolved(cfg) => cfg,
            VarResolution::Missing(m) => panic!("unexpected missing vars: {m:?}"),
        };
        assert_eq!(cfg.deployment["app"].container.image, "myapp:v1.2.3");
    }

    #[test]
    fn resolve_substitutes_bare_traversal() {
        // A bare `var.x` (not inside a "${...}" template) must also resolve.
        let src = r#"
project = "demo"
deployment "app" {
  container {
    image = var.image
  }
}
"#;
        let cfg = match resolve_with(src, &[("image", "nginx:1.27")]) {
            VarResolution::Resolved(cfg) => cfg,
            VarResolution::Missing(m) => panic!("unexpected missing vars: {m:?}"),
        };
        assert_eq!(cfg.deployment["app"].container.image, "nginx:1.27");
    }

    #[test]
    fn resolve_reports_missing_var() {
        let src = r#"
project = "demo"
deployment "app" {
  container {
    image = "myapp:${var.image_tag}"
  }
}
"#;
        match resolve_with(src, &[]) {
            VarResolution::Missing(m) => {
                assert_eq!(m, BTreeSet::from(["image_tag".to_string()]));
            }
            VarResolution::Resolved(_) => panic!("expected missing image_tag"),
        }
    }

    #[test]
    fn resolve_reports_all_missing_vars_at_once() {
        // Two distinct missing vars in separate attributes should both be
        // reported in a single pass, so the caller can prompt for everything.
        let src = r#"
project = "demo"
deployment "app" {
  port = 8080
  container {
    image = "myapp:${var.tag}"
    env = {
      API_URL = var.api_url
    }
  }
}
"#;
        match resolve_with(src, &[]) {
            VarResolution::Missing(m) => {
                assert_eq!(
                    m,
                    BTreeSet::from(["tag".to_string(), "api_url".to_string()])
                );
            }
            VarResolution::Resolved(_) => panic!("expected missing vars"),
        }
    }

    #[test]
    fn resolve_rejects_interpolated_project() {
        // `project` must be a literal: it's read by `destroy` without any var
        // context, so it can't depend on interpolation. Reject it even when the
        // referenced var *is* supplied.
        let src = r#"project = "app-${var.suffix}""#;
        let map = BTreeMap::from([("suffix".to_string(), "prod".to_string())]);
        let err = UpConfig::resolve(Path::new("unisrv.hcl"), src, &map).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("project"), "should name project: {msg}");
        assert!(
            msg.contains("literal"),
            "should explain literal rule: {msg}"
        );
    }

    #[test]
    fn resolve_rejects_unknown_namespace() {
        // `vars` (note the trailing s) is not a declared namespace. This is a
        // genuine error, not a request to prompt for a missing `var` value.
        let src = r#"
project = "demo"
deployment "app" {
  container {
    image = "myapp:${vars.image_tag}"
  }
}
"#;
        let err = UpConfig::resolve(Path::new("unisrv.hcl"), src, &BTreeMap::new()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("vars"),
            "should surface the undefined namespace: {msg}"
        );
    }

    #[test]
    fn eval_error_renders_with_source_pointer() {
        // A typo'd namespace (`vars` instead of `var`) is a real eval error; it
        // should render the cargo-style caret like other config errors, not a
        // bare message.
        let src = r#"
project = "demo"
deployment "app" {
  container {
    image = "x:${vars.tag}"
  }
}
"#;
        let err = UpConfig::resolve(Path::new("unisrv.hcl"), src, &BTreeMap::new()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("vars"), "names the offender: {msg}");
        assert!(msg.contains("-->"), "renders a source pointer: {msg}");
        assert!(msg.contains('^'), "renders a caret under the token: {msg}");
    }

    #[test]
    fn resolve_ignores_unused_vars() {
        // A supplied var that nothing references is harmless (no warning in the
        // MVP) — resolution still succeeds.
        let src = r#"
project = "demo"
deployment "app" {
  container {
    image = "myapp:${var.tag}"
  }
}
"#;
        let cfg = match resolve_with(src, &[("tag", "v1"), ("unused", "whatever")]) {
            VarResolution::Resolved(cfg) => cfg,
            VarResolution::Missing(m) => panic!("unexpected missing vars: {m:?}"),
        };
        assert_eq!(cfg.deployment["app"].container.image, "myapp:v1");
    }

    #[test]
    fn resolve_validates_substituted_values() {
        // The interpolated host is only invalid *after* substitution, so this
        // proves evaluation happens before semantic validation.
        let src = r#"
project = "demo"
service "web" {
  hosts = ["${var.sub}.unisrv.dev"]
}
"#;
        // Hyphenated single label under the base domain is rejected.
        let map = BTreeMap::from([("sub".to_string(), "my-app".to_string())]);
        let err = UpConfig::resolve(Path::new("unisrv.hcl"), src, &map).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("my-app.unisrv.dev"),
            "validation should see the substituted host: {msg}"
        );
    }

    #[test]
    fn parse_project_reads_project_ignoring_var_references() {
        // `destroy` reads only the project name and supplies no vars, so a
        // config whose deployment blocks reference unprovided vars must still
        // yield the project.
        let src = r#"
project = "myproj"
deployment "app" {
  container {
    image = "${var.never_provided}"
  }
}
"#;
        let project = UpConfig::parse_project_at(Path::new("unisrv.hcl"), src).unwrap();
        assert_eq!(project, "myproj");
    }

    #[test]
    fn parse_project_at_runs_shared_structural_validation() {
        // destroy reads `project` through the same structural validation as up,
        // so a malformed config (here: duplicate blocks) is rejected by both —
        // no validation divergence between the two commands. Still needs no vars.
        let src = r#"
project = "myproj"
deployment "app" {
  container { image = "i1" }
}
deployment "app" {
  container { image = "i2" }
}
"#;
        let err = UpConfig::parse_project_at(Path::new("unisrv.hcl"), src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("duplicate"),
            "destroy should reject duplicate blocks like up does; got: {msg}"
        );
    }

    #[test]
    fn rejects_heredoc_project_without_misattributing_interpolation() {
        // A heredoc parses as a template (not a String literal) even with no
        // ${...}, so it's rejected — but the message must not claim the user
        // used interpolation when they didn't.
        let src = "project = <<-EOT\nmyproj\nEOT\n";
        let err = UpConfig::resolve(Path::new("unisrv.hcl"), src, &BTreeMap::new()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("literal"),
            "should explain the literal rule: {msg}"
        );
        assert!(
            !msg.contains("interpolation"),
            "should not misattribute to interpolation: {msg}"
        );
    }

    #[test]
    fn parse_project_rejects_interpolated_project() {
        let src = r#"project = "app-${var.x}""#;
        let err = UpConfig::parse_project_at(Path::new("unisrv.hcl"), src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("project"), "msg: {msg}");
        assert!(msg.contains("literal"), "msg: {msg}");
    }

    #[test]
    fn parses_minimal_nginx_example() {
        let src = r#"
project = "nginx-demo"

service "nginx" {
  hosts = ["nginx.unisrv.dev"]
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
        assert_eq!(
            cfg.service["nginx"].hosts.as_deref(),
            Some(["nginx.unisrv.dev".to_string()].as_slice())
        );

        let dep = &cfg.deployment["nginx"];
        assert_eq!(dep.service.as_deref(), Some("nginx"));
        assert_eq!(dep.port, Some(80));
        assert_eq!(dep.container.image, "nginx");
        assert!(dep.container.args.is_none());
        assert!(dep.container.env.is_none());
    }

    #[test]
    fn parses_bare_service_block_without_hosts() {
        let src = r#"
project = "demo"
service "web" {}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        assert_eq!(cfg.service.len(), 1);
        assert!(cfg.service["web"].hosts.is_none());
    }

    #[test]
    fn rejects_legacy_host_field() {
        // `host` was replaced by `hosts`. Pre-alpha: a plain unknown-field
        // rejection (which still names `host`) is enough — no migration hint.
        let src = r#"
project = "demo"
service "web" {
  host = "web.example.com"
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("host"),
            "should name the rejected field: {msg}"
        );
    }

    #[test]
    fn rejects_hyphenated_base_domain_host() {
        let src = r#"
project = "demo"
service "web" { hosts = ["my-app.unisrv.dev"] }
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("my-app.unisrv.dev"),
            "should name the host: {msg}"
        );
    }

    #[test]
    fn rejects_nested_base_domain_host() {
        let src = r#"
project = "demo"
service "web" { hosts = ["a.b.unisrv.dev"] }
"#;
        let err = UpConfig::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("a.b.unisrv.dev"));
    }

    #[test]
    fn accepts_single_label_base_domain_and_external_hyphens() {
        // Single-label *.unisrv.dev is fine; hyphens are fine OFF the base domain.
        let src = r#"
project = "demo"
service "web" { hosts = ["myapp.unisrv.dev", "my-app.example.com"] }
"#;
        assert!(UpConfig::parse(src).is_ok());
    }

    #[test]
    fn rejects_same_host_bound_to_two_services() {
        // A host binds to exactly one service (the server 409s on a second
        // link), so the same host under two service blocks is a config error.
        let src = r#"
project = "demo"
service "web" { hosts = ["shared.example.com"] }
service "api" { hosts = ["shared.example.com"] }
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("shared.example.com"),
            "should name the host: {msg}"
        );
    }

    #[test]
    fn parses_multiple_custom_hosts_in_order() {
        let src = r#"
project = "demo"
service "web" {
  hosts = ["a.example.com", "b.example.com"]
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        assert_eq!(
            cfg.service["web"].hosts.as_deref(),
            Some(["a.example.com".to_string(), "b.example.com".to_string()].as_slice())
        );
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
            Some([String::from("--config"), String::from("/etc/app.conf")].as_slice(),),
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
service "s" {}
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
service "web" {}
service "web" {}
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
service "" {}
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
    fn parses_network_block_with_iprange() {
        let src = r#"
project = "demo"
network "internal" {
  iprange = "10.1.0.0/24"
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        assert_eq!(cfg.network.len(), 1);
        assert_eq!(
            cfg.network["internal"].iprange.as_deref(),
            Some("10.1.0.0/24")
        );
    }

    #[test]
    fn rejects_invalid_iprange() {
        let src = r#"
project = "demo"
network "internal" {
  iprange = "not-a-cidr"
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not-a-cidr"), "should name the value: {msg}");
        assert!(msg.contains("CIDR"), "should explain the rule: {msg}");
    }

    #[test]
    fn rejects_non_canonical_iprange_suggesting_network_address() {
        // Host bits must be zero (the backend's cidr parse enforces the same),
        // and the error should offer the masked network address as a fix.
        let src = r#"
project = "demo"
network "internal" {
  iprange = "10.0.0.5/16"
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("10.0.0.5/16"), "names the value: {msg}");
        assert!(
            msg.contains("did you mean \"10.0.0.0/16\""),
            "suggests the canonical network address: {msg}"
        );
    }

    #[test]
    fn rejects_deployment_referencing_undefined_network() {
        let src = r#"
project = "x"
deployment "d" {
  network = "ghost"
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ghost"), "names the network: {msg}");
        assert!(msg.contains("not defined"), "explains the rule: {msg}");
    }

    #[test]
    fn parses_deployment_network_reference() {
        let src = r#"
project = "x"
network "internal" {}
deployment "d" {
  network = "internal"
  container { image = "i" }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        assert_eq!(cfg.deployment["d"].network.as_deref(), Some("internal"));
        // A bare network block (no iprange) is valid — the default fills in.
        assert!(cfg.network["internal"].iprange.is_none());
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
