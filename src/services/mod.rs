use crate::{
    config::CliConfig,
    instances::{self, list::InstanceListResponse, resolve_uuid},
    services::new::ServiceInstanceTarget,
};
use anyhow::{Ok, Result};
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

mod delete;
mod info;
mod list;
mod new;
mod target;

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
                .alias("info")
                .about("Get detailed information about a service")
                .arg(
                    Arg::new("service_id")
                        .help("Service UUID or name")
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(
            Command::new("delete")
                .alias("rm")
                .about("Delete a service")
                .arg(
                    Arg::new("service_id")
                        .help("Service UUID or name")
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(
            Command::new("target")
                .about("Manage service targets")
                .subcommand_required(true)
                .subcommand(
                    Command::new("add")
                        .about("Add a target to a service")
                        .arg(
                            Arg::new("service_id")
                                .help("Service UUID or name")
                                .required(true)
                                .index(1),
                        )
                        .arg(
                            Arg::new("target")
                                .help("Instance UUID and internal port, e.g. 123e4567-e89b-12d3-a456-426614174000:8080")
                                .required(true)
                                .index(2),
                        ),
                )
                .subcommand(
                    Command::new("delete")
                        .alias("rm")
                        .about("Delete a target from a service")
                        .arg(
                            Arg::new("service_id")
                                .help("Service UUID or name")
                                .required(true)
                                .index(1),
                        )
                        .arg(
                            Arg::new("target_id")
                                .help("Target UUID")
                                .required(true)
                                .index(2),
                        ),
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
                        )
                )
                .subcommand(
                    Command::new("http")
                        .about("Creates a new HTTP service")
                        .arg(
                            Arg::new("host")
                                .help("Domain host of the HTTP service, e.g. app.example.com")
                                .required(true)
                                .index(1),
                        )
                        .arg(
                            Arg::new("target")
                                .short('t')
                                .help("Instance UUID and internal port, e.g. 123e4567-e89b-12d3-a456-426614174000:8080")
                                .required(true)
                                .action(clap::ArgAction::Append)
                        )
                )
        )
}

pub async fn handle(config: &mut CliConfig, instance_matches: &clap::ArgMatches) -> Result<()> {
    let http_client = Client::new();
    match instance_matches.subcommand() {
        Some(("list", args)) => list::list_services(&http_client, config, args).await,
        Some(("show", args)) => info::get_service_info(&http_client, config, args).await,
        Some(("delete", args)) => delete::delete_service(&http_client, config, args).await,
        Some(("target", target_matches)) => match target_matches.subcommand() {
            Some(("add", args)) => target::add_target(&http_client, config, args).await,
            Some(("delete", args)) => target::delete_target(&http_client, config, args).await,
            _ => {
                eprintln!("Unknown target command");
                Ok(())
            }
        },
        Some(("new", now_matches)) => {
            match now_matches.subcommand() {
                Some(("tcp", args)) => {
                    let name = args.get_one::<String>("name").unwrap();
                    let targets: Vec<String> = args
                        .get_many::<String>("target")
                        .unwrap()
                        .cloned()
                        .collect();

                    let parsed_targets =
                        parse_targets(&targets, instances::list::list(&http_client, config).await?)
                            .await?;

                    let request = new::ServiceProvisionRequest {
                        region: "dev".to_string(),
                        name: name.to_string(),
                        configuration: new::ServiceConfiguration::Tcp,
                        instance_targets: parsed_targets,
                    };
                    let response = new::new_service(request, &http_client, config).await?;
                    println!(
                        "Service created with ID: {}\nConnection String: {}",
                        response.service_id, response.connection_string
                    );
                }
                Some(("http", args)) => {
                    let host = args.get_one::<String>("host").unwrap();
                    let (host, subdomain) = as_domain(host)
                        .map_err(|e| anyhow::anyhow!("Invalid host format: {}", e))?;

                    let targets: Vec<String> = args
                        .get_many::<String>("target")
                        .unwrap()
                        .cloned()
                        .collect();

                    let parsed_targets =
                        parse_targets(&targets, instances::list::list(&http_client, config).await?)
                            .await?;

                    let request = new::ServiceProvisionRequest {
                        region: "dev".to_string(),
                        name: subdomain.to_string(),
                        configuration: new::ServiceConfiguration::Http {
                            host: host.to_string(),
                        },
                        instance_targets: parsed_targets,
                    };
                    let response = new::new_service(request, &http_client, config).await?;
                    println!(
                        "Service created with ID: {} \nHost: https://{host}",
                        response.service_id
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

async fn parse_target(target: &str, list: &InstanceListResponse) -> Result<(Uuid, u16)> {
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
    if let Some(parsed_uuid) = Uuid::parse_str(input).ok() {
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

async fn parse_targets(
    targets: &[String],
    list: InstanceListResponse,
) -> Result<Vec<ServiceInstanceTarget>> {
    let mut parsed_targets: Vec<ServiceInstanceTarget> = Vec::with_capacity(targets.len());
    for target in targets {
        let (id, port) = parse_target(&target, &list).await?;
        parsed_targets.push(ServiceInstanceTarget {
            instance_id: id,
            instance_port: port,
        });
    }
    Ok(parsed_targets)
}

fn as_domain(host: &str) -> Result<(String, String)> {
    if host.ends_with(".unisrv.dev") {
        let subdomain = host.trim_end_matches(".unisrv.dev");
        return Ok((host.to_owned(), subdomain.to_string()));
    }
    if host.contains('.') || host.chars().any(|c| !c.is_alphabetic() || !c.is_numeric()) {
        return Err(anyhow::anyhow!(
            "Invalid host format. Expected single subdomain which will be used as <subdomain>.unisrv.dev"
        ));
    }

    Ok((format!("{}.unisrv.dev", host), host.to_string()))
}
