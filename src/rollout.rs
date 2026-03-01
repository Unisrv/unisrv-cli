use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

use crate::config::CliConfig;
use crate::instances::{self, run, stop};
use crate::services::{self, target};

/// Seconds to keep the log stream open after instance reaches ExecutingContainer
/// before considering it healthy. Will be replaced with proper health checks.
const HEALTH_CHECK_WAIT: Duration = Duration::from_secs(1);

pub fn command() -> Command {
    Command::new("rollout")
        .about("Perform a rolling update of instances behind a service target group")
        .arg(
            Arg::new("service_id")
                .help("Service UUID, name, or UUID prefix")
                .required(true)
                .index(1),
        )
        .arg(
            Arg::new("container_image")
                .help("Container image to run (e.g., 'nginx:latest')")
                .required(true)
                .index(2),
        )
        .arg(
            Arg::new("group")
                .help("Target group name")
                .long("group")
                .short('g')
                .default_value("default"),
        )
        .arg(
            Arg::new("port")
                .help(
                    "Instance port for targets (auto-resolved from existing targets if all agree)",
                )
                .long("port")
                .short('p')
                .value_parser(clap::value_parser!(u16).range(1..=65535)),
        )
        .arg(
            Arg::new("replicas")
                .help(
                    "Number of replicas (defaults to count of existing group instances, minimum 1)",
                )
                .long("replicas")
                .short('r')
                .value_parser(clap::value_parser!(u32).range(1..)),
        )
        .arg(
            Arg::new("vcpu_count")
                .help("Number of vCPUs to allocate [1-32]")
                .long("vcpus")
                .short('c')
                .value_parser(clap::value_parser!(u8).range(1..=32))
                .default_value("1"),
        )
        .arg(
            Arg::new("memory_mb")
                .help("Amount of memory in GB (G) or MB (M) [128M-128G]")
                .long("memory")
                .short('m')
                .value_parser(instances::parse_memory_mb)
                .default_value("1024M"),
        )
        .arg(
            Arg::new("env")
                .help("Environment variables as KEY=VALUE pairs")
                .long("env")
                .short('e')
                .action(clap::ArgAction::Append),
        )
        .arg(
            Arg::new("network")
                .help("Join each instance to a network (IP is auto-allocated per instance): <network_id/name>")
                .long("network")
                .value_name("NETWORK"),
        )
        .arg(
            Arg::new("args")
                .help("Arguments to pass to the container")
                .index(3)
                .num_args(0..)
                .trailing_var_arg(true),
        )
        .arg(
            Arg::new("leave_behind")
                .help(
                    "What to leave behind from the old deployment: \
                    'instances' deregisters targets but keeps instances running; \
                    'targets' keeps both targets and instances untouched",
                )
                .long("leave-behind")
                .value_name("WHAT")
                .value_parser(["instances", "targets"]),
        )
}

/// Generate a 4-char lowercase hex deploy identifier that doesn't conflict
/// with any existing instance name under the `{service_name}_{group}_` prefix.
fn generate_deploy_hex(service_name: &str, group: &str, existing_names: &[String]) -> String {
    loop {
        let bytes = Uuid::new_v4();
        let hex = bytes.simple().to_string();
        let candidate = &hex[..4];
        let prefix = format!("{service_name}_{group}_{candidate}_");
        if !existing_names.iter().any(|n| n.starts_with(&prefix)) {
            return candidate.to_string();
        }
    }
}

/// Best-effort cleanup: stop all given instances, ignoring individual errors.
async fn stop_all(client: &Client, config: &mut CliConfig, instance_ids: &[Uuid]) {
    for &id in instance_ids {
        if let Err(e) = stop::stop_instance(client, config, id, 5000).await {
            log::warn!("Failed to stop instance {} during cleanup: {}", id, e);
        }
    }
}

pub async fn handle(
    config: &mut CliConfig,
    client: &Client,
    args: &clap::ArgMatches,
) -> Result<()> {
    config.ensure_auth()?;
    let service_id_str = args.get_one::<String>("service_id").unwrap();
    let container_image = args.get_one::<String>("container_image").unwrap().as_str();
    let group = args.get_one::<String>("group").unwrap().clone();
    let vcpu_count = *args.get_one::<u8>("vcpu_count").unwrap();
    let memory_mb = *args.get_one::<u16>("memory_mb").unwrap() as u32;
    let env = instances::parse_env_vars(args.get_many::<String>("env"))?;
    let network = args.get_one::<String>("network").cloned();
    let container_args: Option<Vec<String>> = args
        .get_many::<String>("args")
        .map(|v| v.cloned().collect());
    let requested_replicas = args.get_one::<u32>("replicas").cloned();
    let requested_port = args.get_one::<u16>("port").cloned();
    let leave_behind = args.get_one::<String>("leave_behind").map(|s| s.as_str());

    // Resolve service ID
    let service_id =
        services::resolve_service_id(service_id_str, &services::list::list(client, config).await?)?;

    // Fetch service info (name + existing targets)
    let service_info = {
        let response = client
            .get(config.url(&format!("/service/{service_id}")))
            .bearer_auth(config.token(client).await?)
            .send()
            .await?;
        crate::error::check_response(response, "fetch service info")
            .await?
            .json::<services::info::ServiceInfoResponse>()
            .await?
    };

    let service_name = service_info.name.clone();

    // Filter existing targets by group
    let old_targets: Vec<&services::info::ServiceTarget> = service_info
        .targets
        .iter()
        .filter(|t| {
            let tg = t.target_group.as_deref().unwrap_or("default");
            tg == group
        })
        .collect();

    // Resolve replica count
    let replicas = requested_replicas
        .map(|r| r as usize)
        .unwrap_or_else(|| old_targets.len().max(1));

    // Resolve port
    let port = if let Some(p) = requested_port {
        p
    } else if !old_targets.is_empty() {
        let ports: HashSet<u16> = old_targets.iter().map(|t| t.instance_port).collect();
        if ports.len() == 1 {
            *ports.iter().next().unwrap()
        } else {
            return Err(anyhow::anyhow!(
                "--port required: existing targets in group '{}' have different ports",
                group
            ));
        }
    } else {
        return Err(anyhow::anyhow!(
            "--port required when no existing targets exist for group '{}'",
            group
        ));
    };

    // Fetch all instance names for deploy hex uniqueness check
    let all_instances = instances::list::list(client, config).await?;
    let existing_names: Vec<String> = all_instances
        .instances
        .iter()
        .filter_map(|i| i.name.clone())
        .collect();

    // Generate unique deploy hex
    let deploy_hex = generate_deploy_hex(&service_name, &group, &existing_names);

    // [1/5] Verify container image
    let pb = crate::default_spinner();
    pb.set_message(format!("[1/5] Verifying {}...", container_image));
    let scoped_token = run::verify_and_get_token(container_image, config)
        .await
        .inspect_err(|_| pb.finish_and_clear())?;
    pb.finish_and_clear();

    // [2/5] Start new instances one by one
    let mut new_instance_ids: Vec<Uuid> = Vec::with_capacity(replicas);

    for i in 0..replicas {
        let name = format!("{service_name}_{group}_{deploy_hex}_{i}");

        let pb = crate::default_spinner();
        pb.set_prefix(format!("[2/5 {}/{}]", i + 1, replicas));
        pb.set_message(format!("Provisioning {}...", name));

        let params = run::RunInstanceParams {
            container_image,
            vcpu_count,
            memory_mb,
            args: container_args.clone(),
            env: env.clone(),
            name: Some(name.clone()),
            network: network.clone(),
        };

        let instance_id =
            match run::create_instance(client, config, &params, scoped_token.clone()).await {
                Ok(id) => id,
                Err(e) => {
                    pb.finish_and_clear();
                    stop_all(client, config, &new_instance_ids).await;
                    return Err(e);
                }
            };

        new_instance_ids.push(instance_id);
        let short_id = instance_id.to_string()[..8].to_string();
        pb.set_prefix(format!("[2/5 {}/{}] {short_id}", i + 1, replicas));
        pb.set_message("Starting...");

        if let Err(e) = instances::logs::stream_logs_until_running(
            client,
            config,
            instance_id,
            Some(pb),
            HEALTH_CHECK_WAIT,
        )
        .await
        {
            stop_all(client, config, &new_instance_ids).await;
            return Err(e);
        }
    }

    // [3/5] Add new targets to service
    let pb = crate::default_spinner();
    pb.set_message(format!(
        "[3/5] Adding {} target(s) to service (group: {})...",
        replicas, group
    ));
    for &instance_id in &new_instance_ids {
        if let Err(e) =
            target::create_target(client, config, service_id, instance_id, port, &group).await
        {
            pb.finish_and_clear();
            stop_all(client, config, &new_instance_ids).await;
            return Err(e);
        }
    }
    pb.finish_and_clear();

    // [4/5] Deregister old targets, [5/5] stop old instances
    if !old_targets.is_empty() {
        if leave_behind != Some("targets") {
            let pb = crate::default_spinner();
            pb.set_message(format!(
                "[4/5] Deregistering {} old target(s)...",
                old_targets.len()
            ));
            for old_target in &old_targets {
                if let Err(e) =
                    target::remove_target(client, config, service_id, old_target.id).await
                {
                    log::warn!("Failed to remove old target {}: {}", old_target.id, e);
                }
            }
            pb.finish_and_clear();
        }

        if leave_behind.is_none() {
            let pb = crate::default_spinner();
            pb.set_message(format!(
                "[5/5] Stopping {} old instance(s)...",
                old_targets.len()
            ));
            for old_target in &old_targets {
                if let Err(e) =
                    stop::stop_instance(client, config, old_target.instance_id, 5000).await
                {
                    log::warn!(
                        "Failed to stop old instance {}: {}",
                        old_target.instance_id,
                        e
                    );
                }
            }
            pb.finish_and_clear();
        }
    }

    println!(
        "\u{2705} Rolled out {} replica(s) of group '{}' on service '{}'.",
        replicas, group, service_name
    );

    Ok(())
}
