//! Typed view of `unisrv.hcl`.
//!
//! Parsing goes through `hcl-rs`: source is parsed to a structural `hcl::Body`,
//! `${var.X}` references are evaluated against caller-supplied variables, and
//! the result is deserialized into these typed structs via serde. See
//! [`UpConfig::resolve`]; command-line variable handling lives in [`super::vars`].

use anyhow::{Context, Result};
use hcl::eval::Evaluate;
use indexmap::IndexMap;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::defaults::DEFAULT_LOCATION_PATH;
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
    /// Serve plain HTTP as well as HTTPS. Off by default: HTTP requests are
    /// permanently redirected to HTTPS instead.
    #[serde(default)]
    pub allow_http: Option<bool>,
    /// Shorthand for `location "/" { deployment = "…" }`. Desugars to a
    /// catch-all appended *after* every explicit location, so it never shadows
    /// them under the proxy's first-match-wins order.
    #[serde(default)]
    pub deployment: Option<String>,
    /// Routing table, keyed by path prefix (the block label). Declaration order
    /// is preserved — the proxy matches locations one by one, first match wins.
    #[serde(default, rename = "location")]
    pub locations: IndexMap<String, LocationBlock>,
}

/// A `location "PATH" { … }` block inside a service: routes requests whose path
/// starts with PATH to exactly one target — a deployment reference, a raw
/// instance group, or an external URL.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LocationBlock {
    /// Name of a `deployment` block to route to. The reference *is* the service
    /// binding: the deployment joins the instance group named after it.
    #[serde(default)]
    pub deployment: Option<String>,
    /// Raw instance-group name to route to, without binding any deployment.
    /// Escape hatch for groups populated out-of-band (instances API).
    #[serde(default)]
    pub instance_group: Option<String>,
    /// External URL to proxy to. Unlike instance targets, the location's path
    /// prefix is stripped before forwarding.
    #[serde(default)]
    pub url: Option<String>,
    /// Path (plus optional query) to re-route to within the same target when
    /// the upstream responds 404 — e.g. "/index.html" for SPA fallback.
    #[serde(default)]
    pub override_404: Option<String>,
}

/// The single resolved target of a location. A [`LocationBlock`] is parsed with
/// three optional target attributes; validation requires exactly one, and this
/// enum is that choice — making "exactly one" unrepresentable past the boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum LocationTarget {
    /// Route to (and bind) the named deployment's instance group.
    Deployment(String),
    /// Route to a raw instance group without binding any deployment.
    InstanceGroup(String),
    /// Proxy to an external URL.
    Url(String),
}

/// A location after desugaring: a service's explicit `location` blocks in
/// declaration order, followed by the `deployment` shorthand as a catch-all
/// "/" appended last. Both `validate` and `DesiredState::from_config` consume
/// this single representation so they never drift on routing semantics.
#[derive(Debug)]
pub struct ResolvedLocation<'a> {
    pub path: &'a str,
    pub override_404: Option<&'a str>,
    /// `None` only for a malformed location that does not set exactly one
    /// target — a state `validate` rejects, so post-validation consumers
    /// (`from_config`) may `expect` it.
    pub target: Option<LocationTarget>,
}

impl LocationBlock {
    /// The single resolved target, or `None` when not exactly one of
    /// `deployment`/`instance_group`/`url` is set.
    fn target(&self) -> Option<LocationTarget> {
        match (&self.deployment, &self.instance_group, &self.url) {
            (Some(d), None, None) => Some(LocationTarget::Deployment(d.clone())),
            (None, Some(g), None) => Some(LocationTarget::InstanceGroup(g.clone())),
            (None, None, Some(u)) => Some(LocationTarget::Url(u.clone())),
            _ => None,
        }
    }
}

impl ServiceBlock {
    /// The service's routing table, desugared: explicit locations in
    /// declaration order, then the `deployment` shorthand as a catch-all "/"
    /// appended last (first-match-wins, so it never shadows explicit routes).
    pub fn resolved_locations(&self) -> Vec<ResolvedLocation<'_>> {
        let mut out: Vec<ResolvedLocation<'_>> = self
            .locations
            .iter()
            .map(|(path, loc)| ResolvedLocation {
                path,
                override_404: loc.override_404.as_deref(),
                target: loc.target(),
            })
            .collect();
        if let Some(dep) = &self.deployment {
            out.push(ResolvedLocation {
                path: DEFAULT_LOCATION_PATH,
                override_404: None,
                target: Some(LocationTarget::Deployment(dep.clone())),
            });
        }
        out
    }

    /// Deployment names this service routes to — and therefore binds: explicit
    /// `location` deployment refs plus the `deployment` shorthand. The single
    /// source of truth for service→deployment bindings.
    pub fn referenced_deployments(&self) -> impl Iterator<Item = &str> {
        self.locations
            .values()
            .filter_map(|loc| loc.deployment.as_deref())
            .chain(self.deployment.as_deref())
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentBlock {
    /// Port that the container listens on. Required when a service location
    /// references this deployment.
    #[serde(default)]
    pub port: Option<u16>,
    /// Name of a `network` block whose network all instances join (optional).
    /// The referenced network must be defined in this file.
    #[serde(default)]
    pub network: Option<String>,
    /// Number of vCPUs per instance (1–32). Optional — defaults to
    /// [`super::defaults::DEFAULT_VCPU_COUNT`]. Parsed wide; `validate`
    /// enforces the range, so post-validation consumers may narrow.
    #[serde(default)]
    pub vcpus: Option<u64>,
    /// Guaranteed share of a physical core per vCPU. Optional — defaults to
    /// [`super::defaults::DEFAULT_VCPU_RATIO`]; `validate` restricts it to the
    /// scheduler's discrete tiers.
    #[serde(default)]
    pub vcpu_ratio: Option<f64>,
    /// Number of instances to run (0–10; 0 keeps the deployment defined but
    /// runs nothing). Optional — defaults to
    /// [`super::defaults::DEFAULT_REPLICAS`].
    #[serde(default)]
    pub replicas: Option<u64>,
    /// Memory per instance (128MB–32GB). A bare number is megabytes; a string
    /// takes an MB/M/GB/G suffix ("512MB", "2G"). Optional — defaults to
    /// [`super::defaults::DEFAULT_MEMORY_MB`].
    #[serde(default)]
    pub memory: Option<MemoryAttr>,
    pub container: ContainerBlock,
}

/// The `memory` attribute as written: HCL allows a bare number (megabytes) or
/// a human-readable string with a unit suffix. [`Self::to_mb`] is the single
/// conversion; `validate` runs it so post-validation consumers may `expect`.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(
    untagged,
    expecting = "a number of MB or a string like \"512MB\" or \"2GB\""
)]
pub enum MemoryAttr {
    /// Bare number: megabytes.
    Mb(u64),
    /// String with a unit suffix, e.g. "512MB", "512M", "2GB", "2g".
    Spec(String),
}

impl MemoryAttr {
    /// Megabytes, or a message explaining why the spec doesn't parse. Units
    /// are binary (1GB = 1024MB), case-insensitive; a fractional value is fine
    /// as long as it lands on a whole number of MB ("1.5GB" = 1536).
    pub fn to_mb(&self) -> Result<u64, String> {
        let spec = match self {
            MemoryAttr::Mb(mb) => return Ok(*mb),
            MemoryAttr::Spec(s) => s,
        };
        let upper = spec.trim().to_ascii_uppercase();
        let (number, factor) = if let Some(n) = upper.strip_suffix("MB") {
            (n, 1.0)
        } else if let Some(n) = upper.strip_suffix("GB") {
            (n, 1024.0)
        } else if let Some(n) = upper.strip_suffix('M') {
            (n, 1.0)
        } else if let Some(n) = upper.strip_suffix('G') {
            (n, 1024.0)
        } else {
            return Err(format!(
                "{spec:?} has no unit suffix; write a string like \"512MB\" or \"2GB\" \
                 (or a bare number of MB)"
            ));
        };
        let value: f64 = number.trim_end().parse().map_err(|_| {
            format!("{spec:?} is not a valid memory size (e.g. \"512MB\", \"1.5GB\")")
        })?;
        if !value.is_finite() || value <= 0.0 {
            return Err(format!("{spec:?} must be a positive memory size"));
        }
        let mb = value * factor;
        if mb.fract() != 0.0 {
            return Err(format!("{spec:?} is not a whole number of MB ({mb}MB)"));
        }
        Ok(mb as u64)
    }
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

    /// Non-fatal warnings about a *valid* config that is probably not what the
    /// user meant. Printed by `up` before planning; never blocks an apply.
    pub fn lints(&self) -> Vec<String> {
        let mut lints = Vec::new();
        for (svc_name, svc) in &self.service {
            for (path, loc) in &svc.locations {
                if let Some(group) = &loc.instance_group
                    && self.deployment.contains_key(group)
                {
                    lints.push(format!(
                        "location \"{path}\" in service \"{svc_name}\" routes to instance_group \
                         \"{group}\", which matches a deployment block but does not bind it — \
                         use `deployment = \"{group}\"` if you meant to route to that deployment"
                    ));
                }
            }
        }
        lints
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
        for (svc_name, svc) in &self.service {
            // The shorthand `deployment` is desugared into this list (a "/"
            // catch-all appended last), so every check below sees the same
            // routing table the proxy and `from_config` will.
            let resolved = svc.resolved_locations();
            for loc in &resolved {
                let path = loc.path;
                if let Some(reason) = invalid_location_path(path) {
                    return Err(err(
                        format!("location \"{path}\" in service \"{svc_name}\": {reason}"),
                        Some(Locator::substring(&format!("location \"{path}\""))),
                    ));
                }
                let Some(target) = &loc.target else {
                    return Err(err(
                        format!(
                            "location \"{path}\" in service \"{svc_name}\" must have exactly one \
                             of `deployment`, `instance_group` or `url`"
                        ),
                        Some(Locator::substring(&format!("location \"{path}\""))),
                    ));
                };
                if let Some(o404) = loc.override_404
                    && let Some(reason) = invalid_override_404(o404)
                {
                    return Err(err(
                        format!(
                            "`override_404` in location \"{path}\" of service \"{svc_name}\": {reason}"
                        ),
                        Some(Locator::substring(&format!("\"{o404}\""))),
                    ));
                }
                if let LocationTarget::Url(url) = target
                    && let Some(reason) = invalid_url_target(url)
                {
                    return Err(err(
                        format!("`url` in location \"{path}\" of service \"{svc_name}\": {reason}"),
                        Some(Locator::substring(&format!("\"{url}\""))),
                    ));
                }
            }
            // The same path twice — including the shorthand "/" colliding with
            // an explicit one — can never both be reached.
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for loc in &resolved {
                if !seen.insert(loc.path) {
                    return Err(err(
                        format!(
                            "service \"{svc_name}\" defines location \"{}\" more than once",
                            loc.path
                        ),
                        Some(Locator::substring(&format!("location \"{}\"", loc.path))),
                    ));
                }
            }
            // First match wins in the proxy: a location whose path extends an
            // earlier one can never be reached.
            let paths: Vec<&str> = resolved.iter().map(|l| l.path).collect();
            for (i, &later) in paths.iter().enumerate() {
                if let Some(&earlier) = paths[..i].iter().find(|&&e| later.starts_with(e)) {
                    return Err(err(
                        format!(
                            "location \"{later}\" in service \"{svc_name}\" is unreachable: \
                             \"{earlier}\" is declared before it and matches those requests first \
                             — declare the more specific path first"
                        ),
                        Some(Locator::substring(&format!("location \"{later}\""))),
                    ));
                }
            }
        }
        // Both explicit location refs and the shorthand must resolve to a
        // defined deployment with a port, bound to at most one service.
        let mut routed: BTreeMap<&str, &str> = BTreeMap::new();
        for (svc_name, svc) in &self.service {
            for dep_name in svc.referenced_deployments() {
                let Some(dep) = self.deployment.get(dep_name) else {
                    return Err(err(
                        format!(
                            "service \"{svc_name}\" routes to deployment \"{dep_name}\" which is not defined"
                        ),
                        Some(Locator::substring(&format!("\"{dep_name}\""))),
                    ));
                };
                if dep.port.is_none() {
                    return Err(err(
                        format!(
                            "deployment \"{dep_name}\" is routed by service \"{svc_name}\" but has no `port` set"
                        ),
                        Some(Locator::substring(&format!("deployment \"{dep_name}\""))),
                    ));
                }
                if let Some(first) = routed.insert(dep_name, svc_name)
                    && first != svc_name.as_str()
                {
                    return Err(err(
                        format!(
                            "deployment \"{dep_name}\" is routed from multiple services \
                             (\"{first}\" and \"{svc_name}\"); a deployment can bind to only one service"
                        ),
                        Some(Locator::substring(&format!("\"{dep_name}\"")).nth(1)),
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
            if let Some(vcpus) = dep.vcpus
                && !(MIN_VCPUS..=MAX_VCPUS).contains(&vcpus)
            {
                return Err(err(
                    format!(
                        "`vcpus` in deployment \"{name}\" must be between 1 and 32, got {vcpus}"
                    ),
                    Some(Locator::substring(&vcpus.to_string())),
                ));
            }
            if let Some(replicas) = dep.replicas
                && replicas > MAX_REPLICAS
            {
                return Err(err(
                    format!(
                        "`replicas` in deployment \"{name}\" must be between 0 and 10, got {replicas}"
                    ),
                    Some(Locator::substring(&replicas.to_string())),
                ));
            }
            if let Some(ratio) = dep.vcpu_ratio
                && !VCPU_RATIO_TIERS.contains(&ratio)
            {
                return Err(err(
                    format!(
                        "`vcpu_ratio` in deployment \"{name}\" must be one of 0.125, 0.25, 0.5 \
                         or 1.0, got {ratio}"
                    ),
                    Some(Locator::substring("vcpu_ratio")),
                ));
            }
            if let Some(memory) = &dep.memory {
                let needle = match memory {
                    MemoryAttr::Spec(s) => format!("\"{s}\""),
                    MemoryAttr::Mb(n) => n.to_string(),
                };
                match memory.to_mb() {
                    Err(reason) => {
                        return Err(err(
                            format!("`memory` in deployment \"{name}\": {reason}"),
                            Some(Locator::substring(&needle)),
                        ));
                    }
                    Ok(mb) if !(MIN_MEMORY_MB..=MAX_MEMORY_MB).contains(&mb) => {
                        return Err(err(
                            format!(
                                "`memory` in deployment \"{name}\" must be between 128MB and \
                                 32GB, got {mb}MB"
                            ),
                            Some(Locator::substring(&needle)),
                        ));
                    }
                    Ok(_) => {}
                }
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

/// Returns an error message if `path` is not a usable location path prefix,
/// else `None`. The proxy matches locations by raw `starts_with` against the
/// request path with no normalization, so a prefix must look like the start of
/// a request path: leading `/`, no query/fragment, no whitespace, no empty
/// segments. A trailing slash is allowed — `/api/` (subtree only) and `/api`
/// (subtree plus the bare path) are distinct, intentional routes.
fn invalid_location_path(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return Some("path must start with \"/\"".into());
    }
    if let Some(c) = path
        .chars()
        .find(|c| matches!(c, '?' | '#') || c.is_whitespace())
    {
        return Some(format!(
            "path must not contain {c:?}; a location is a path prefix, not a URL"
        ));
    }
    if path.contains("//") {
        return Some("path must not contain \"//\" (the proxy does not normalize paths)".into());
    }
    None
}

/// Returns an error message if `value` is not usable as an `override_404`,
/// else `None`. The proxy re-routes within the same target by parsing the
/// value as a path-and-query (it cannot jump to another host), so a full URL
/// would be treated as a nonsense literal path. Parsed with the same `http`
/// crate as the proxy, so the two agree exactly on character validity.
fn invalid_override_404(value: &str) -> Option<String> {
    if !value.starts_with('/') {
        return Some(format!(
            "{value:?} must be a path on the same target starting with \"/\" \
             (e.g. \"/index.html\"), not a full URL"
        ));
    }
    if value.contains("//") {
        return Some(format!(
            "{value:?} must not contain \"//\"; a leading \"//\" is a protocol-relative \
             host reference, and the proxy does not normalize paths"
        ));
    }
    match value.parse::<http::uri::PathAndQuery>() {
        Ok(_) => None,
        Err(e) => Some(format!("{value:?} is not a valid path: {e}")),
    }
}

/// Returns an error message if `url` is not an absolute http(s) URL, else
/// `None`. The proxy resolves the target host from the URL's authority, so a
/// relative value has nowhere to go. Parsed with the same `http` crate as the
/// proxy.
fn invalid_url_target(url: &str) -> Option<String> {
    let parsed: http::Uri = match url.parse() {
        Ok(uri) => uri,
        Err(e) => return Some(format!("{url:?} is not a valid URL: {e}")),
    };
    let scheme_ok = matches!(parsed.scheme_str(), Some("http") | Some("https"));
    // `authority().is_some()` is true even for a host-less authority like
    // ":8080"; require an actual non-empty host the proxy can connect to.
    let host_ok = parsed.host().is_some_and(|h| !h.is_empty());
    if scheme_ok && host_ok {
        None
    } else {
        Some(format!(
            "{url:?} must be an absolute URL like \"https://host\" or \"http://host/path\""
        ))
    }
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

/// Per-instance resource bounds, mirroring the scheduler's limits so the CLI
/// fails fast with a source span instead of waiting for an API 400.
const MIN_MEMORY_MB: u64 = 128;
const MAX_MEMORY_MB: u64 = 32 * 1024;
const MIN_VCPUS: u64 = 1;
const MAX_VCPUS: u64 = 32;
/// Discrete core-share tiers the scheduler supports. All powers of two, so
/// exact f64 comparison is sound.
const VCPU_RATIO_TIERS: [f64; 4] = [0.125, 0.25, 0.5, 1.0];
/// 0 is allowed: the deployment stays defined but runs no instances.
const MAX_REPLICAS: u64 = 10;

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
///
/// Duplicates are scoped to their parent body: two services may each declare a
/// `location "/"`, but the same path twice *within* one service is rejected.
fn validate_blocks(path: &Path, source: &str, body: &hcl::Body) -> Result<(), ConfigParseError> {
    let mut seen: BTreeSet<(&str, Vec<&str>)> = BTreeSet::new();
    for block in body.blocks() {
        let kind = block.identifier();
        let labels: Vec<&str> = block.labels().iter().map(|l| l.as_str()).collect();

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

        validate_blocks(path, source, block.body())?;
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

  location "/" {
    deployment = "nginx"
  }
}

deployment "nginx" {
  port = 80
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
        assert_eq!(
            cfg.service["nginx"].locations["/"].deployment.as_deref(),
            Some("nginx")
        );

        let dep = &cfg.deployment["nginx"];
        assert_eq!(dep.port, Some(80));
        assert_eq!(dep.container.image, "nginx");
        assert!(dep.container.args.is_none());
        assert!(dep.container.env.is_none());
    }

    #[test]
    fn parses_location_block_with_deployment_target() {
        let src = r#"
project = "demo"
service "web" {
  location "/api" {
    deployment = "api"
  }
}
deployment "api" {
  port = 8000
  container { image = "i" }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        let locations = &cfg.service["web"].locations;
        assert_eq!(locations.len(), 1);
        assert_eq!(locations["/api"].deployment.as_deref(), Some("api"));
    }

    #[test]
    fn preserves_location_declaration_order() {
        // The proxy walks locations in order (first match wins), so the parsed
        // table must come back in file order — not sorted.
        let src = r#"
project = "demo"
service "web" {
  location "/zzz" { deployment = "a" }
  location "/api" { deployment = "b" }
  location "/aaa" { deployment = "c" }
}
deployment "a" {
  port = 1
  container { image = "i" }
}
deployment "b" {
  port = 2
  container { image = "i" }
}
deployment "c" {
  port = 3
  container { image = "i" }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        let paths: Vec<&str> = cfg.service["web"]
            .locations
            .keys()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(paths, vec!["/zzz", "/api", "/aaa"]);
    }

    #[test]
    fn rejects_location_with_two_targets() {
        let src = r#"
project = "demo"
service "web" {
  location "/api" {
    deployment     = "api"
    instance_group = "canary"
  }
}
deployment "api" {
  port = 8000
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exactly one"), "states the rule: {msg}");
        assert!(msg.contains("/api"), "names the location: {msg}");
    }

    #[test]
    fn rejects_location_with_no_target() {
        let src = r#"
project = "demo"
service "web" {
  location "/api" {}
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exactly one"), "states the rule: {msg}");
        assert!(
            msg.contains("deployment") && msg.contains("instance_group") && msg.contains("url"),
            "lists the target attributes: {msg}"
        );
    }

    #[test]
    fn rejects_location_referencing_undefined_deployment() {
        let src = r#"
project = "demo"
service "web" {
  location "/api" { deployment = "ghost" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ghost"), "names the deployment: {msg}");
        assert!(msg.contains("not defined"), "explains the rule: {msg}");
    }

    #[test]
    fn rejects_shorthand_referencing_undefined_deployment() {
        let src = r#"
project = "demo"
service "web" {
  deployment = "ghost"
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ghost"), "names the deployment: {msg}");
        assert!(msg.contains("not defined"), "explains the rule: {msg}");
    }

    #[test]
    fn rejects_routed_deployment_without_port() {
        // The binding forwards traffic to the container's port; a routed
        // deployment without one has nowhere to receive it.
        let src = r#"
project = "demo"
service "web" {
  location "/" { deployment = "app" }
}
deployment "app" {
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("port"), "names the missing field: {msg}");
        assert!(msg.contains("app"), "names the deployment: {msg}");
    }

    #[test]
    fn rejects_deployment_routed_from_two_services() {
        // A deployment binds to exactly one service (the binding is singular
        // on the backend), so references from two services are a config error.
        let src = r#"
project = "demo"
service "a" {
  location "/" { deployment = "app" }
}
service "b" {
  deployment = "app"
}
deployment "app" {
  port = 80
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("app"), "names the deployment: {msg}");
        assert!(
            msg.contains("\"a\"") && msg.contains("\"b\""),
            "names both services: {msg}"
        );
    }

    #[test]
    fn rejects_malformed_location_paths() {
        // The proxy matches raw string prefixes with no normalization, so
        // anything that can't be a request-path prefix is a config bug.
        for (path, why) in [
            ("api", "missing leading slash"),
            ("/api?x=1", "query string"),
            ("/api#frag", "fragment"),
            ("/api docs", "whitespace"),
            ("/api//v2", "double slash"),
        ] {
            let src = format!(
                r#"
project = "demo"
service "web" {{
  location "{path}" {{ instance_group = "g" }}
}}
"#
            );
            let err = UpConfig::parse(&src).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains(path),
                "({why}) should name the path {path:?}: {msg}"
            );
        }
    }

    #[test]
    fn accepts_trailing_slash_location_path() {
        // "/api/" is a distinct, intentional route: it matches "/api/…" but not
        // bare "/api" (and not "/apifoo") — so it must not be rejected or stripped.
        let src = r#"
project = "demo"
service "web" {
  location "/api/" { instance_group = "g" }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        assert!(cfg.service["web"].locations.contains_key("/api/"));
    }

    #[test]
    fn rejects_unreachable_shadowed_location() {
        // The proxy walks locations in declared order, first match wins: with
        // "/" declared first, "/api" can never match.
        let src = r#"
project = "demo"
service "web" {
  location "/" { instance_group = "front" }
  location "/api" { instance_group = "api" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unreachable"), "states the problem: {msg}");
        assert!(
            msg.contains("/api") && msg.contains("\"/\""),
            "names both locations: {msg}"
        );
    }

    #[test]
    fn accepts_specific_location_before_catchall() {
        let src = r#"
project = "demo"
service "web" {
  location "/api" { instance_group = "api" }
  location "/" { instance_group = "front" }
}
"#;
        assert!(UpConfig::parse(src).is_ok());
    }

    #[test]
    fn rejects_shorthand_alongside_explicit_root_location() {
        // The shorthand desugars to a "/" location appended after the explicit
        // ones, so declaring an explicit "/" too is a duplicate path.
        let src = r#"
project = "demo"
service "web" {
  deployment = "app"
  location "/" { instance_group = "g" }
}
deployment "app" {
  port = 80
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("more than once"),
            "states the duplicate: {msg}"
        );
        assert!(msg.contains("location \"/\""), "names the path: {msg}");
    }

    #[test]
    fn accepts_same_location_path_in_different_services() {
        let src = r#"
project = "demo"
service "a" {
  location "/" { instance_group = "g1" }
}
service "b" {
  location "/" { instance_group = "g2" }
}
"#;
        assert!(UpConfig::parse(src).is_ok());
    }

    #[test]
    fn rejects_duplicate_location_paths_within_a_service() {
        let src = r#"
project = "demo"
service "web" {
  location "/api" { instance_group = "g1" }
  location "/api" { instance_group = "g2" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("duplicate"), "states the problem: {msg}");
        assert!(msg.contains("/api"), "names the path: {msg}");
    }

    #[test]
    fn rejects_override_404_that_is_not_a_path() {
        // The proxy parses override_404 as a path+query to re-route within the
        // same upstream — a full URL (scheme/host) silently fails there.
        let src = r#"
project = "demo"
service "web" {
  location "/" {
    instance_group = "g"
    override_404   = "https://other.example.com/fallback"
  }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("override_404"), "names the field: {msg}");
        assert!(
            msg.contains("path"),
            "explains it must be a path, not a URL: {msg}"
        );
    }

    #[test]
    fn rejects_override_404_with_double_slash() {
        // "//evil.com/login" is a protocol-relative authority, not a path on the
        // same target; the proxy does not normalize, so it must be rejected just
        // like a location path with "//".
        let src = r#"
project = "demo"
service "web" {
  location "/" {
    instance_group = "g"
    override_404   = "//evil.com/login"
  }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("override_404"), "names the field: {msg}");
        assert!(
            msg.contains("//"),
            "explains the double-slash problem: {msg}"
        );
    }

    #[test]
    fn rejects_non_absolute_url_target() {
        for bad in ["old.example.com", "/internal", "ftp://files.example.com"] {
            let src = format!(
                r#"
project = "demo"
service "web" {{
  location "/legacy" {{ url = "{bad}" }}
}}
"#
            );
            let err = UpConfig::parse(&src).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains(bad), "names the value {bad:?}: {msg}");
            assert!(
                msg.contains("http://") || msg.contains("https://"),
                "should show the expected shape: {msg}"
            );
        }
    }

    #[test]
    fn rejects_url_target_with_empty_host() {
        // "http://:8080" parses as a valid URI with an authority but no host;
        // the proxy resolves the target host from the authority, so a host-less
        // URL has nowhere to connect and must be rejected at parse time.
        let src = r#"
project = "demo"
service "web" {
  location "/legacy" { url = "http://:8080" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("http://:8080"), "names the value: {msg}");
        assert!(
            msg.contains("http://") || msg.contains("https://"),
            "shows the expected shape: {msg}"
        );
    }

    #[test]
    fn lints_instance_group_matching_a_deployment_name() {
        // `instance_group` routes to a raw group WITHOUT binding the
        // deployment — if a deployment by that name exists, the user almost
        // certainly wanted `deployment =`. Warn, don't error: it stays valid
        // for groups genuinely populated out-of-band.
        let src = r#"
project = "demo"
service "web" {
  location "/api" { instance_group = "api" }
}
deployment "api" {
  port = 8000
  container { image = "i" }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        let lints = cfg.lints();
        assert_eq!(lints.len(), 1);
        assert!(lints[0].contains("instance_group"), "lint: {}", lints[0]);
        assert!(
            lints[0].contains("deployment = \"api\""),
            "suggests the fix: {}",
            lints[0]
        );
    }

    #[test]
    fn no_lints_for_unrelated_instance_group() {
        let src = r#"
project = "demo"
service "web" {
  location "/canary" { instance_group = "canary" }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        assert!(cfg.lints().is_empty());
    }

    #[test]
    fn resolves_vars_inside_location_blocks() {
        let src = r#"
project = "demo"
service "web" {
  location "/legacy" {
    url = "https://${var.legacy_host}/v1"
  }
}
"#;
        let cfg = match resolve_with(src, &[("legacy_host", "old.example.com")]) {
            VarResolution::Resolved(cfg) => cfg,
            VarResolution::Missing(m) => panic!("unexpected missing vars: {m:?}"),
        };
        assert_eq!(
            cfg.service["web"].locations["/legacy"].url.as_deref(),
            Some("https://old.example.com/v1")
        );
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
    fn rejects_service_attr_on_deployment() {
        // Binding moved to the service side: `deployment.service` is gone.
        // Pre-alpha: a plain unknown-field rejection (which still names
        // `service`) is enough — no migration hint.
        let src = r#"
project = "demo"
service "web" {}
deployment "web" {
  service = "web"
  port    = 80
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("service"), "should name the field: {msg}");
        assert!(
            msg.contains("unknown"),
            "should be a plain unknown-field rejection: {msg}"
        );
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
    fn memory_type_mismatch_error_shows_expected_shapes() {
        // An untagged-enum miss must not leak "did not match any variant";
        // the user should see what `memory` accepts.
        let src = r#"
project = "demo"
deployment "api" {
  memory = true
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("512MB"), "shows the expected shape: {msg}");
        assert!(!msg.contains("untagged"), "no serde internals: {msg}");
    }

    #[test]
    fn rejects_unparseable_memory_specs() {
        for (spec, why) in [
            ("512XB", "unknown unit"),
            ("512", "string without unit"),
            ("1.3GB", "not a whole number of MB"),
            ("MB", "no number"),
            ("-1GB", "negative"),
        ] {
            let src = format!(
                r#"
project = "demo"
deployment "api" {{
  memory = "{spec}"
  container {{ image = "i" }}
}}
"#
            );
            let err = UpConfig::parse(&src).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains(spec), "({why}) should name the value: {msg}");
        }
    }

    #[test]
    fn rejects_memory_out_of_bounds() {
        for spec in ["64", "\"127MB\"", "\"33GB\""] {
            let src = format!(
                r#"
project = "demo"
deployment "api" {{
  memory = {spec}
  container {{ image = "i" }}
}}
"#
            );
            let err = UpConfig::parse(&src).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("128MB") && msg.contains("32GB"),
                "({spec}) should state the bounds: {msg}"
            );
        }
    }

    #[test]
    fn accepts_memory_at_bounds() {
        for spec in ["128", "\"128MB\"", "\"32GB\""] {
            let src = format!(
                r#"
project = "demo"
deployment "api" {{
  memory = {spec}
  container {{ image = "i" }}
}}
"#
            );
            assert!(UpConfig::parse(&src).is_ok(), "({spec}) should be valid");
        }
    }

    #[test]
    fn rejects_vcpus_out_of_bounds() {
        for n in [0, 33] {
            let src = format!(
                r#"
project = "demo"
deployment "api" {{
  vcpus = {n}
  container {{ image = "i" }}
}}
"#
            );
            let err = UpConfig::parse(&src).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("between 1 and 32"),
                "({n}) should state the bounds: {msg}"
            );
            assert!(msg.contains("vcpus"), "({n}) should name the field: {msg}");
        }
    }

    #[test]
    fn accepts_vcpus_at_bounds() {
        for n in [1, 32] {
            let src = format!(
                r#"
project = "demo"
deployment "api" {{
  vcpus = {n}
  container {{ image = "i" }}
}}
"#
            );
            assert!(UpConfig::parse(&src).is_ok(), "({n}) should be valid");
        }
    }

    #[test]
    fn rejects_vcpu_ratio_off_tier() {
        for r in ["0.3", "0.0", "2.0"] {
            let src = format!(
                r#"
project = "demo"
deployment "api" {{
  vcpu_ratio = {r}
  container {{ image = "i" }}
}}
"#
            );
            let err = UpConfig::parse(&src).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("vcpu_ratio"), "({r}) names the field: {msg}");
            assert!(
                msg.contains("0.125")
                    && msg.contains("0.25")
                    && msg.contains("0.5")
                    && msg.contains('1'),
                "({r}) should list the valid tiers: {msg}"
            );
        }
    }

    #[test]
    fn accepts_all_vcpu_ratio_tiers() {
        // `1` (integer literal) must coerce like `1.0` — users will write both.
        for r in ["0.125", "0.25", "0.5", "1.0", "1"] {
            let src = format!(
                r#"
project = "demo"
deployment "api" {{
  vcpu_ratio = {r}
  container {{ image = "i" }}
}}
"#
            );
            assert!(UpConfig::parse(&src).is_ok(), "({r}) should be valid");
        }
    }

    #[test]
    fn rejects_replicas_above_bound() {
        let src = r#"
project = "demo"
deployment "api" {
  replicas = 11
  container { image = "i" }
}
"#;
        let err = UpConfig::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("replicas"), "names the field: {msg}");
        assert!(msg.contains("between 0 and 10"), "states the bounds: {msg}");
    }

    #[test]
    fn accepts_replicas_at_bounds_including_scale_to_zero() {
        for n in [0, 10] {
            let src = format!(
                r#"
project = "demo"
deployment "api" {{
  replicas = {n}
  container {{ image = "i" }}
}}
"#
            );
            assert!(UpConfig::parse(&src).is_ok(), "({n}) should be valid");
        }
    }

    #[test]
    fn parses_bare_deployment_without_port() {
        // A deployment nothing routes to (a worker) needs no port.
        let src = r#"
project = "x"
deployment "worker" {
  container { image = "worker:1" }
}
"#;
        let cfg = UpConfig::parse(src).unwrap();
        assert!(cfg.deployment["worker"].port.is_none());
    }
}
