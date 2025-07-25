use crate::{
    config::CliConfig,
    instances::{self, list::InstanceListResponse, resolve_uuid},
    services::new::ServiceInstanceTarget,
};
use anyhow::Result;
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

mod info;
mod list;
mod new;

pub fn command() -> Command {
    Command::new("service")
        .alias("srv")
        .alias("services")
        .about("Manage services")
        .subcommand_required(false)
        .subcommand(
            Command::new("list")
                .about("List all services")
                .alias("ls"),
        )
        .subcommand(
            Command::new("show")
                .alias("get")
                .about("Get detailed information about a service")
                .arg(
                    Arg::new("service_id")
                        .help("Service UUID or name")
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(
            Command::new("new")
                .about("Creates a new service")
                .subcommand_required(true)
                .subcommand(
                    Command::new("tcp")
                        .about("Creates a new TCP service")
                        .arg(
                            Arg::new("name")
                                .help("Name of the service")
                                .required(true)
                                .index(1),
                        )
                        .arg(
                            Arg::new("target")
                                .short('t')
                                .help("Instance UUID and internal port, e.g. 123e4567-e89b-12d3-a456-426614174000:8080")
                                .required(true) //for now.
                                .action(clap::ArgAction::Append)
                        ),
                ),
        )
}

pub async fn handle(config: &mut CliConfig, instance_matches: &clap::ArgMatches) -> Result<()> {
    let http_client = Client::new();
    match instance_matches.subcommand() {
        Some(("list", args)) => list::list_services(&http_client, config, args).await,
        Some(("show", args)) => info::get_service_info(&http_client, config, args).await,
        Some(("new", now_matches)) => {
            match now_matches.subcommand() {
                Some(("tcp", args)) => {
                    let name = args.get_one::<String>("name").unwrap();
                    let targets: Vec<String> = args
                        .get_many::<String>("target")
                        .unwrap()
                        .cloned()
                        .collect();

                    let mut parsed_targets: Vec<ServiceInstanceTarget> =
                        Vec::with_capacity(targets.len());
                    for target in targets {
                        let (id, port) = parse_target(
                            &target,
                            instances::list::list(&http_client, config).await?,
                        )
                        .await?;
                        parsed_targets.push(ServiceInstanceTarget {
                            instance_id: id,
                            instance_port: port,
                        });
                    }

                    let request = new::ServiceProvisionRequest {
                        region: "dev".to_string(),
                        name: name.to_string(),
                        configuration: new::ServiceConfiguration::TCP,
                        instance_targets: parsed_targets,
                    };
                    let response = new::new_service(request, &http_client, config).await?;
                    println!(
                        "Service created with ID: {}\nConnection String: {}",
                        response.service_id, response.connection_string
                    );
                }
                _ => {
                    eprintln!("Unknown service command");
                }
            }
            Ok(())
        }
        Some((_, _)) => {
            eprintln!("Unknown service command");
            Ok(())
        }
        None => {
            // Default to listing services when no subcommand is provided
            list::list_services(&http_client, config, &clap::ArgMatches::default()).await
        }
    }
}

async fn parse_target(target: &str, list: InstanceListResponse) -> Result<(Uuid, u16)> {
    let parts: Vec<&str> = target.split(':').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!(
            "Invalid instance target format. Expected UUID:port"
        ));
    }
    let uuid = resolve_uuid(parts[0], list).await.map_err(|e| {
        anyhow::anyhow!("Failed to resolve target instance UUID {}: {}", parts[0], e)
    })?;
    let port = parts[1].parse::<u16>()?;

    Ok((uuid, port))
}

pub async fn resolve_service_id(input: &str, list: list::ServiceListResponse) -> Result<Uuid> {
    // First try to parse as UUID
    if let Ok(parsed_uuid) = Uuid::parse_str(input) {
        return Ok(parsed_uuid);
    }

    // Try to find by name (exact match)
    for service in &list.services {
        if service.name == input {
            return Ok(service.id);
        }
    }

    // If not a valid UUID and no name match, check if it could be a UUID prefix
    if input.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let starts_with_input = list
            .services
            .iter()
            .filter(|service| service.id.to_string().starts_with(input))
            .collect::<Vec<_>>();

        if starts_with_input.len() == 1 {
            return Ok(starts_with_input[0].id);
        } else if starts_with_input.is_empty() {
            return Err(anyhow::anyhow!(
                "No service found with UUID starting with '{}'",
                input
            ));
        } else {
            return Err(anyhow::anyhow!(
                "Multiple services ({}) found with UUID starting with '{}'.",
                starts_with_input.len(),
                input
            ));
        }
    }

    Err(anyhow::anyhow!(
        "No service found with name '{}' or UUID '{}'",
        input,
        input
    ))
}
