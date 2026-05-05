//! Field-by-field diff and render for [`DeploymentConfiguration`].
//!
//! Every field of the struct must appear in the destructure pattern. Adding
//! a field to the upstream type produces a compile error here until handled,
//! preventing silent omissions in the plan output.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use unisrv_api::models::DeploymentConfiguration;

pub fn render_config_diff(
    out: &mut String,
    current: &DeploymentConfiguration,
    desired: &DeploymentConfiguration,
) {
    let DeploymentConfiguration {
        replicas: c_replicas,
        region: c_region,
        container_image: c_container_image,
        args: c_args,
        env: c_env,
        vcpu_ratio: c_vcpu_ratio,
        vcpu_count: c_vcpu_count,
        memory_mb: c_memory_mb,
        network: c_network,
        instance_port: c_instance_port,
    } = current;
    let DeploymentConfiguration {
        replicas: d_replicas,
        region: d_region,
        container_image: d_container_image,
        args: d_args,
        env: d_env,
        vcpu_ratio: d_vcpu_ratio,
        vcpu_count: d_vcpu_count,
        memory_mb: d_memory_mb,
        network: d_network,
        instance_port: d_instance_port,
    } = desired;

    if c_container_image != d_container_image {
        let _ = writeln!(
            out,
            "      image:    {c_container_image} -> {d_container_image}"
        );
    }
    if c_replicas != d_replicas {
        let _ = writeln!(out, "      replicas: {c_replicas} -> {d_replicas}");
    }
    if c_region != d_region {
        let _ = writeln!(out, "      region:   {c_region} -> {d_region}");
    }
    if c_instance_port != d_instance_port {
        let _ = writeln!(
            out,
            "      port:     {} -> {}",
            opt_display(c_instance_port.as_ref()),
            opt_display(d_instance_port.as_ref()),
        );
    }
    if c_args != d_args {
        let _ = writeln!(
            out,
            "      args:     {} -> {}",
            args_display(c_args.as_deref()),
            args_display(d_args.as_deref()),
        );
    }
    if c_env != d_env {
        render_env_diff(out, c_env.as_ref(), d_env.as_ref());
    }
    if c_network != d_network {
        let _ = writeln!(
            out,
            "      network:  {} -> {}",
            c_network.as_deref().unwrap_or("<unset>"),
            d_network.as_deref().unwrap_or("<unset>"),
        );
    }
    if (c_vcpu_count, c_vcpu_ratio, c_memory_mb) != (d_vcpu_count, d_vcpu_ratio, d_memory_mb) {
        let _ = writeln!(
            out,
            "      resources: {c_vcpu_count}vcpu @ {c_vcpu_ratio} / {c_memory_mb}MB -> {d_vcpu_count}vcpu @ {d_vcpu_ratio} / {d_memory_mb}MB"
        );
    }
}

fn opt_display<T: std::fmt::Display>(v: Option<&T>) -> String {
    match v {
        Some(v) => v.to_string(),
        None => "<unset>".into(),
    }
}

fn args_display(v: Option<&[String]>) -> String {
    match v {
        Some(args) => format!("{args:?}"),
        None => "<unset>".into(),
    }
}

fn render_env_diff(
    out: &mut String,
    current: Option<&BTreeMap<String, String>>,
    desired: Option<&BTreeMap<String, String>>,
) {
    let empty = BTreeMap::new();
    let c = current.unwrap_or(&empty);
    let d = desired.unwrap_or(&empty);
    let keys: BTreeSet<&String> = c.keys().chain(d.keys()).collect();

    let mut entries: Vec<String> = Vec::new();
    for k in keys {
        match (c.get(k), d.get(k)) {
            (None, Some(v)) => entries.push(format!("+{k}={v}")),
            (Some(v), None) => entries.push(format!("-{k}={v}")),
            (Some(cv), Some(dv)) if cv != dv => entries.push(format!("~{k}: {cv} -> {dv}")),
            _ => {}
        }
    }
    if !entries.is_empty() {
        let _ = writeln!(out, "      env:      {}", entries.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> DeploymentConfiguration {
        DeploymentConfiguration {
            replicas: 1,
            region: "dev".into(),
            container_image: "nginx:1".into(),
            args: None,
            env: None,
            vcpu_ratio: 0.25,
            vcpu_count: 1,
            memory_mb: 256,
            network: None,
            instance_port: Some(80),
        }
    }

    #[test]
    fn renders_image_change() {
        let mut out = String::new();
        let c = base();
        let mut d = base();
        d.container_image = "nginx:2".into();
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("image:    nginx:1 -> nginx:2"), "got: {out}");
    }

    #[test]
    fn renders_port_set_to_unset() {
        let mut out = String::new();
        let c = base();
        let mut d = base();
        d.instance_port = None;
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("port:     80 -> <unset>"), "got: {out}");
    }

    #[test]
    fn renders_port_unset_to_set() {
        let mut out = String::new();
        let mut c = base();
        c.instance_port = None;
        let d = base();
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("port:     <unset> -> 80"), "got: {out}");
    }

    #[test]
    fn renders_args_change() {
        let mut out = String::new();
        let mut c = base();
        let mut d = base();
        c.args = Some(vec!["--quiet".into()]);
        d.args = Some(vec!["--quiet".into(), "--debug".into()]);
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("args:"), "got: {out}");
        assert!(out.contains("--quiet"), "got: {out}");
        assert!(out.contains("--debug"), "got: {out}");
    }

    #[test]
    fn renders_env_diff_with_add_remove_modify() {
        let mut out = String::new();
        let mut c_env = BTreeMap::new();
        c_env.insert("LOG_LEVEL".to_string(), "info".to_string());
        c_env.insert("OLD_VAR".to_string(), "stale".to_string());
        let mut d_env = BTreeMap::new();
        d_env.insert("LOG_LEVEL".to_string(), "debug".to_string());
        d_env.insert("NEW_VAR".to_string(), "fresh".to_string());

        let mut c = base();
        let mut d = base();
        c.env = Some(c_env);
        d.env = Some(d_env);
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("env:"), "got: {out}");
        assert!(out.contains("+NEW_VAR=fresh"), "got: {out}");
        assert!(out.contains("-OLD_VAR=stale"), "got: {out}");
        assert!(out.contains("~LOG_LEVEL: info -> debug"), "got: {out}");
    }

    #[test]
    fn renders_resources_grouped() {
        let mut out = String::new();
        let c = base();
        let mut d = base();
        d.vcpu_count = 2;
        d.memory_mb = 512;
        render_config_diff(&mut out, &c, &d);
        assert!(
            out.contains("resources: 1vcpu @ 0.25 / 256MB -> 2vcpu @ 0.25 / 512MB"),
            "got: {out}"
        );
    }

    #[test]
    fn renders_network_change() {
        let mut out = String::new();
        let c = base();
        let mut d = base();
        d.network = Some("internal".into());
        render_config_diff(&mut out, &c, &d);
        assert!(out.contains("network:  <unset> -> internal"), "got: {out}");
    }

    #[test]
    fn no_output_when_unchanged() {
        let mut out = String::new();
        let c = base();
        render_config_diff(&mut out, &c.clone(), &c);
        assert_eq!(out, "");
    }
}
