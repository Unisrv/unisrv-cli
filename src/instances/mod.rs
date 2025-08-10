pub mod list;
mod logs;
mod run;
mod show;
mod stop;

use std::collections::HashMap;

use crate::{config::CliConfig, instances::list::RUNNING_STATE};
use anyhow::Result;
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

pub fn command() -> Command {
    Command::new("instance")
        .alias("vm")
        .alias("instances")
        .about("Manage instances")
        .subcommand_required(false)
        .subcommand(
            Command::new("run")
                .about("Run a new instance with a specified container image")
                .arg(
                    Arg::new("container_image")
                        .help("The container image to run (e.g., 'nginx:latest')")
                        .required(true),
                )
                .arg(
                    Arg::new("vcpu_count")
                        .help("Number of vCPUs to allocate for the instance [1-32]")
                        .long("vcpus")
                        .short('c')
                        .value_parser(clap::value_parser!(u8).range(1..=32))
                        .allow_negative_numbers(false)
                        .default_value("1"),
                )
                .arg(
                    Arg::new("memory_mb")
                        .help("Amount of memory in GB (G) or MB (M) to allocate for the instance [128M-128G]")
                        .long("memory")
                        .short('m')
                        .value_parser(parse_memory_mb)
                        .allow_negative_numbers(false)
                        .default_value("1024M"),
                )
                .arg(
                    Arg::new("env")
                        .help("Environment variables to set in the instance, specified as KEY=VALUE pairs")
                        .long("env")
                        .short('e')
                )
                .arg(
                    Arg::new("name")
                        .help("Optional name for the instance")
                        .long("name")
                        .short('n')
                        .value_name("NAME")
                )
                .arg(
                    Arg::new("network")
                        .help("Join instance to network, format: [ip]@<network_id/name> (IP is optional - will auto-assign if omitted)")
                        .long("network")
                        .value_name("[IP]@NETWORK")
                )
                .arg(
                    Arg::new("args")
                        .help("Arguments to pass to the container")
                        .num_args(0..)
                        .trailing_var_arg(true),
                ),
        )
        .subcommand(
            Command::new("stop")
                .alias("rm")
                .about("Stop an instance by UUID, name, or UUID prefix")
                .arg(
                    Arg::new("uuid")
                        .help("The UUID, name, or UUID prefix of the instance to terminate")
                        .required(true),
                ).arg(
                    Arg::new("timeout")
                        .help("Graceful shutdown timeout in milliseconds")
                        .long("timeout")
                        .short('t')
                        .value_parser(clap::value_parser!(u32).range(0..=600_000))
                        .default_value("5000")
                        .allow_negative_numbers(false),
                ),
        )
        .subcommand(Command::new("list").about("List all instances").alias("ls").arg(
            Arg::new("include_stopped")
                .help("Include stopped instances in the list")
                .long("include-stopped")
                .short('a')
                .action(clap::ArgAction::SetTrue),
        ))
        .subcommand(
            Command::new("show")
                .about("Show detailed information about a specific instance")
                .alias("get")
                .alias("info")
                .arg_required_else_help(true)
                .arg(
                    Arg::new("instance_id")
                        .help("The UUID, name, or UUID prefix of the instance to show")
                        .required(true),
                )
        )
        .subcommand(
            Command::new("logs")
                .about("Stream logs for a specific instance")
                .alias("log")
                .arg_required_else_help(true)
                .arg(
                    Arg::new("uuid")
                        .help("The UUID, name, or UUID prefix of the instance to stream logs for")
                        .required(true),
                )
            )
}

pub async fn handle(config: &mut CliConfig, instance_matches: &clap::ArgMatches) -> Result<()> {
    let http_client = Client::new();
    match instance_matches.subcommand() {
        Some(("run", run_matches)) => {
            config.ensure_auth()?;
            let vcpu_count = *run_matches.get_one::<u8>("vcpu_count").unwrap();
            let memory_mb = *run_matches.get_one::<u16>("memory_mb").unwrap();
            let env = parse_env_vars(run_matches.get_many::<String>("env"))?;
            let args = run_matches
                .get_many::<String>("args")
                .map(|v| v.cloned().collect());
            let name = run_matches.get_one::<String>("name").cloned();
            let network = run_matches.get_one::<String>("network").cloned();
            run::run_instance(
                &http_client,
                config,
                run::RunInstanceParams {
                    container_image: run_matches
                        .get_one::<String>("container_image")
                        .expect("Container image should be required"),
                    vcpu_count,
                    memory_mb: memory_mb as u32,
                    args,
                    env,
                    name,
                    network,
                },
            )
            .await
        }
        Some(("stop", rm_matches)) => {
            config.ensure_auth()?;
            let uuid = rm_matches
                .get_one::<String>("uuid")
                .expect("UUID should be required?");
            let uuid = resolve_uuid(uuid, &list::list(&http_client, config).await?).await?;
            let timeout_ms = rm_matches
                .get_one::<u32>("timeout")
                .cloned()
                .unwrap_or(5_000);
            stop::stop_instance(&http_client, config, uuid, timeout_ms).await
        }
        Some(("list", list_matches)) => {
            config.ensure_auth()?;
            list::list_instances(
                &http_client,
                config,
                !list_matches
                    .get_one::<bool>("include_stopped")
                    .unwrap_or(&false),
            )
            .await
        }
        Some(("show", show_matches)) => {
            config.ensure_auth()?;
            show::show_instance(&http_client, config, show_matches).await
        }
        Some(("logs", logs_matches)) => {
            config.ensure_auth()?;
            let uuid = logs_matches
                .get_one::<String>("uuid")
                .expect("UUID should be required");
            let uuid = resolve_uuid(uuid, &list::list(&http_client, config).await?).await?;
            logs::stream_logs(&http_client, config, uuid, None).await
        }
        Some((_, _)) => Err(anyhow::anyhow!("Unknown instance command")),
        None => {
            // Default to listing instances when no subcommand is provided
            config.ensure_auth()?;
            list::list_instances(&http_client, config, true).await
        }
    }
}

fn parse_env_vars(
    env_vars: Option<clap::parser::ValuesRef<'_, String>>,
) -> Result<Option<HashMap<String, String>>> {
    if let Some(vars) = env_vars {
        let mut env_map = HashMap::new();
        for var in vars {
            let parts: Vec<&str> = var.splitn(2, '=').collect();
            if parts.len() != 2 {
                return Err(anyhow::anyhow!(
                    "Invalid environment variable format: {}. Expected KEY=VALUE format.",
                    var
                ));
            }
            env_map.insert(parts[0].to_string(), parts[1].to_string());
        }
        Ok(Some(env_map))
    } else {
        Ok(None)
    }
}

fn parse_memory_mb(s: &str) -> Result<u16, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Memory value cannot be empty".to_string());
    }
    let (num_str, unit) = match s.chars().last() {
        Some(c) if !c.is_ascii_digit() => {
            let unit = c.to_ascii_uppercase();
            let num_str = &s[..s.len() - 1];
            if !num_str.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(
                    "Memory value must be a number followed by an optional unit (M/G)".to_string(),
                );
            }
            (num_str, unit)
        }
        _ => {
            if !s.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(
                    "Memory value must be a number, optionally followed by an unit (M/G)"
                        .to_string(),
                );
            }
            (s, 'M') // Default to MB if no suffix
        }
    };
    let num: u32 = num_str
        .parse()
        .map_err(|_| format!("Invalid number: {num_str}"))?;
    let mb = match unit {
        'M' => num,
        'G' => num
            .checked_mul(1024)
            .ok_or("Memory must be between 128M and 128G")?,
        _ => return Err(format!("Invalid memory unit: {unit}")),
    };
    if !(128..=131072).contains(&mb) {
        return Err(format!("Memory must be between 128M and 128G ({mb} MB)"));
    }
    Ok(mb as u16)
}

pub async fn resolve_uuid(input: &str, list: &list::InstanceListResponse) -> Result<Uuid> {
    // First try to parse as UUID
    if let Ok(parsed_uuid) = Uuid::parse_str(input) {
        return Ok(parsed_uuid);
    }

    // Try to find by name (exact match) - only check running instances
    for instance in &list.instances {
        if instance.state == RUNNING_STATE {
            if let Some(ref name) = instance.name {
                if name == input {
                    return Ok(instance.id);
                }
            }
        }
    }

    // If not a valid UUID and no name match, check if it could be a UUID prefix
    if input.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let starts_with_input = list
            .instances
            .iter()
            .filter(|instance| {
                instance.state == RUNNING_STATE && instance.id.to_string().starts_with(input)
            })
            .collect::<Vec<_>>();

        if starts_with_input.len() == 1 {
            return Ok(starts_with_input[0].id);
        } else if starts_with_input.is_empty() {
            return Err(anyhow::anyhow!(
                "No running instance found with UUID starting with '{}'",
                input
            ));
        } else {
            return Err(anyhow::anyhow!(
                "Multiple instances ({}) found with UUID starting with '{}'.",
                starts_with_input.len(),
                input
            ));
        }
    }

    Err(anyhow::anyhow!(
        "No running instance found with name '{}' or UUID '{}'",
        input,
        input
    ))
}
