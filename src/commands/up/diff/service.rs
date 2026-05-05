//! Field-by-field diff and render for services.
//!
//! Two responsibilities:
//!  * [`immutable_diffs`] — detect changes to fields the backend can't update
//!    in place; these become [`RecreateReason::ImmutableField`] entries.
//!  * [`render_config_diff`] — pretty-print the difference between two
//!    [`HTTPServiceConfig`] values, including a path-keyed walk of locations.
//!
//! Every site that reads from `DesiredService`, `CurrentService`,
//! `HTTPServiceConfig`, `HTTPLocation`, or `HTTPLocationTarget` does so via
//! struct/enum destructuring. Adding a field anywhere in this chain fails to
//! compile here until handled.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use unisrv_api::models::{HTTPLocation, HTTPLocationTarget, HTTPServiceConfig};

use crate::commands::up::desired::DesiredService;
use crate::commands::up::plan::{CurrentService, RecreateReason};

/// Returns one `RecreateReason::ImmutableField` per immutable field that
/// differs between desired and current. Mutability is hard-coded here per
/// backend constraints; see plan.rs module docs.
pub fn immutable_diffs(desired: &DesiredService, current: &CurrentService) -> Vec<RecreateReason> {
    let DesiredService {
        name: _,
        host: d_host,
        region: d_region,
        configuration: _,
    } = desired;
    let CurrentService {
        id: _,
        name: _,
        host: c_host,
        region: c_region,
        configuration: _,
    } = current;

    let mut out = Vec::new();
    if d_host != c_host {
        out.push(RecreateReason::ImmutableField {
            field: "host",
            old: c_host.clone(),
            new: d_host.clone(),
        });
    }
    if d_region != c_region {
        out.push(RecreateReason::ImmutableField {
            field: "region",
            old: c_region.clone(),
            new: d_region.clone(),
        });
    }
    out
}

pub fn render_config_diff(
    out: &mut String,
    current: &HTTPServiceConfig,
    desired: &HTTPServiceConfig,
) {
    let HTTPServiceConfig {
        locations: c_locations,
        allow_http: c_allow_http,
    } = current;
    let HTTPServiceConfig {
        locations: d_locations,
        allow_http: d_allow_http,
    } = desired;

    if c_allow_http != d_allow_http {
        let _ = writeln!(out, "      allow_http: {c_allow_http} -> {d_allow_http}");
    }
    if c_locations != d_locations {
        render_locations_diff(out, c_locations, d_locations);
    }
}

fn render_locations_diff(out: &mut String, current: &[HTTPLocation], desired: &[HTTPLocation]) {
    let c_by_path: BTreeMap<&str, &HTTPLocation> =
        current.iter().map(|l| (l.path.as_str(), l)).collect();
    let d_by_path: BTreeMap<&str, &HTTPLocation> =
        desired.iter().map(|l| (l.path.as_str(), l)).collect();
    let all_paths: BTreeSet<&str> = c_by_path.keys().chain(d_by_path.keys()).copied().collect();

    let mut header_written = false;
    for path in all_paths {
        match (c_by_path.get(path), d_by_path.get(path)) {
            (None, Some(d)) => {
                write_header(out, &mut header_written);
                let _ = writeln!(out, "        + {path}");
                render_location_full(out, "            ", d);
            }
            (Some(c), None) => {
                write_header(out, &mut header_written);
                let _ = writeln!(out, "        - {path}");
                render_location_full(out, "            ", c);
            }
            (Some(c), Some(d)) if c != d => {
                write_header(out, &mut header_written);
                let _ = writeln!(out, "        ~ {path}");
                render_location_diff(out, "            ", c, d);
            }
            _ => {}
        }
    }
}

fn write_header(out: &mut String, written: &mut bool) {
    if !*written {
        let _ = writeln!(out, "      locations:");
        *written = true;
    }
}

fn render_location_diff(
    out: &mut String,
    indent: &str,
    current: &HTTPLocation,
    desired: &HTTPLocation,
) {
    let HTTPLocation {
        path: c_path,
        override_404: c_override_404,
        target: c_target,
    } = current;
    let HTTPLocation {
        path: d_path,
        override_404: d_override_404,
        target: d_target,
    } = desired;

    if c_path != d_path {
        let _ = writeln!(out, "{indent}path: {c_path} -> {d_path}");
    }
    if c_override_404 != d_override_404 {
        let cs = c_override_404.as_deref().unwrap_or("<unset>");
        let ds = d_override_404.as_deref().unwrap_or("<unset>");
        let _ = writeln!(out, "{indent}override_404: {cs} -> {ds}");
    }
    if c_target != d_target {
        render_target_diff(out, indent, c_target, d_target);
    }
}

fn render_target_diff(
    out: &mut String,
    indent: &str,
    current: &HTTPLocationTarget,
    desired: &HTTPLocationTarget,
) {
    // Pair-destructuring forces exhaustive coverage of every variant cross.
    // Adding a new `HTTPLocationTarget` variant breaks the build here.
    match (current, desired) {
        (HTTPLocationTarget::Instance { group: c }, HTTPLocationTarget::Instance { group: d }) => {
            let _ = writeln!(out, "{indent}target: instance({c}) -> instance({d})");
        }
        (HTTPLocationTarget::Url { url: c }, HTTPLocationTarget::Url { url: d }) => {
            let _ = writeln!(out, "{indent}target: url({c}) -> url({d})");
        }
        (HTTPLocationTarget::Instance { group: c }, HTTPLocationTarget::Url { url: d }) => {
            let _ = writeln!(out, "{indent}target: instance({c}) -> url({d})");
        }
        (HTTPLocationTarget::Url { url: c }, HTTPLocationTarget::Instance { group: d }) => {
            let _ = writeln!(out, "{indent}target: url({c}) -> instance({d})");
        }
    }
}

fn render_location_full(out: &mut String, indent: &str, loc: &HTTPLocation) {
    let HTTPLocation {
        path: _,
        override_404,
        target,
    } = loc;
    if let Some(v) = override_404 {
        let _ = writeln!(out, "{indent}override_404: {v}");
    }
    match target {
        HTTPLocationTarget::Instance { group } => {
            let _ = writeln!(out, "{indent}target: instance({group})");
        }
        HTTPLocationTarget::Url { url } => {
            let _ = writeln!(out, "{indent}target: url({url})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn cfg(allow_http: bool, locations: Vec<HTTPLocation>) -> HTTPServiceConfig {
        HTTPServiceConfig {
            allow_http,
            locations,
        }
    }

    fn loc(path: &str, target: HTTPLocationTarget) -> HTTPLocation {
        HTTPLocation {
            path: path.into(),
            override_404: None,
            target,
        }
    }

    fn instance(group: &str) -> HTTPLocationTarget {
        HTTPLocationTarget::Instance {
            group: group.into(),
        }
    }

    fn url(url: &str) -> HTTPLocationTarget {
        HTTPLocationTarget::Url { url: url.into() }
    }

    #[test]
    fn renders_allow_http_change() {
        let mut out = String::new();
        render_config_diff(&mut out, &cfg(false, vec![]), &cfg(true, vec![]));
        assert!(out.contains("allow_http: false -> true"), "got: {out}");
    }

    #[test]
    fn renders_added_location() {
        let mut out = String::new();
        let c = cfg(false, vec![]);
        let d = cfg(false, vec![loc("/api", instance("default"))]);
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("locations:"), "got: {out}");
        assert!(out.contains("+ /api"), "got: {out}");
        assert!(out.contains("target: instance(default)"), "got: {out}");
    }

    #[test]
    fn renders_removed_location() {
        let mut out = String::new();
        let c = cfg(false, vec![loc("/old", url("https://old.example"))]);
        let d = cfg(false, vec![]);
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("- /old"), "got: {out}");
        assert!(
            out.contains("target: url(https://old.example)"),
            "got: {out}"
        );
    }

    #[test]
    fn renders_modified_location_target() {
        let mut out = String::new();
        let c = cfg(false, vec![loc("/", instance("default"))]);
        let d = cfg(false, vec![loc("/", url("https://upstream"))]);
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("~ /"), "got: {out}");
        assert!(
            out.contains("target: instance(default) -> url(https://upstream)"),
            "got: {out}"
        );
    }

    #[test]
    fn renders_modified_location_override_404() {
        let mut out = String::new();
        let mut a = loc("/", instance("default"));
        let mut b = loc("/", instance("default"));
        a.override_404 = Some("/404.html".into());
        b.override_404 = None;
        let c = cfg(false, vec![a]);
        let d = cfg(false, vec![b]);
        render_config_diff(&mut out, &c, &d);
        assert!(
            out.contains("override_404: /404.html -> <unset>"),
            "got: {out}"
        );
    }

    #[test]
    fn no_output_when_unchanged() {
        let mut out = String::new();
        let same = cfg(false, vec![loc("/", instance("default"))]);
        render_config_diff(&mut out, &same.clone(), &same);
        assert_eq!(out, "");
    }

    #[test]
    fn immutable_diffs_detects_host_change() {
        let desired = DesiredService {
            name: "web".into(),
            host: "new.example".into(),
            region: "dev".into(),
            configuration: cfg(false, vec![]),
        };
        let current = CurrentService {
            id: Uuid::new_v4(),
            name: "web".into(),
            host: "old.example".into(),
            region: "dev".into(),
            configuration: cfg(false, vec![]),
        };
        let diffs = immutable_diffs(&desired, &current);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(
            diffs[0],
            RecreateReason::ImmutableField { field: "host", .. }
        ));
    }

    #[test]
    fn immutable_diffs_detects_region_change() {
        let desired = DesiredService {
            name: "web".into(),
            host: "h.example".into(),
            region: "us-east".into(),
            configuration: cfg(false, vec![]),
        };
        let current = CurrentService {
            id: Uuid::new_v4(),
            name: "web".into(),
            host: "h.example".into(),
            region: "dev".into(),
            configuration: cfg(false, vec![]),
        };
        let diffs = immutable_diffs(&desired, &current);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(
            diffs[0],
            RecreateReason::ImmutableField {
                field: "region",
                ..
            }
        ));
    }

    #[test]
    fn immutable_diffs_empty_when_only_config_differs() {
        let desired = DesiredService {
            name: "web".into(),
            host: "h.example".into(),
            region: "dev".into(),
            configuration: cfg(true, vec![]),
        };
        let current = CurrentService {
            id: Uuid::new_v4(),
            name: "web".into(),
            host: "h.example".into(),
            region: "dev".into(),
            configuration: cfg(false, vec![]),
        };
        assert!(immutable_diffs(&desired, &current).is_empty());
    }
}
