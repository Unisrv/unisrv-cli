use crate::{
    config::CliConfig,
    instances::{list::InstanceListResponse, resolve_uuid},
};
use anyhow::{Ok, Result};
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

mod delete;
mod info;
mod list;
mod location;
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
                                .help("Instance UUID and internal port, e.g. uuid:8080")
                                .required(true)
                                .index(2),
                        )
                        .arg(
                            Arg::new("group")
                                .long("group")
                                .short('g')
                                .help("Target group name [default: default]")
                                .required(false),
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
                                .help("Target UUID or prefix (omit to select interactively)")
                                .required(false)
                                .index(2),
                        ),
                ),
        )
        .subcommand(
            Command::new("new")
                .about("Creates a new HTTP service")
                .arg(
                    Arg::new("name")
                        .help("Name of the service")
                        .required(true)
                        .index(1),
                )
                .arg(
                    Arg::new("host")
                        .help("Domain host of the HTTP service, e.g. app.example.com or subdomain (will be <subdomain>.unisrv.dev)")
                        .required(true)
                        .index(2),
                )
                .arg(
                    Arg::new("allow_http")
                        .long("allow-http")
                        .help("Allow HTTP connections (default is HTTPS only)")
                        .action(clap::ArgAction::SetTrue)
                )
        )
        .subcommand(
            Command::new("location")
                .alias("loc")
                .about("Manage service locations")
                .subcommand_required(false)
                .arg(
                    Arg::new("service_id")
                        .help("Service UUID or name (defaults to list locations)")
                        .required(false)
                        .index(1),
                )
                .subcommand(
                    Command::new("list")
                        .alias("ls")
                        .about("List all locations for a service")
                        .arg(
                            Arg::new("service_id")
                                .help("Service UUID or name")
                                .required(true)
                                .index(1),
                        ),
                )
                .subcommand(
                    Command::new("add")
                        .about("Add a location to a service")
                        .arg(
                            Arg::new("service_id")
                                .help("Service UUID or name")
                                .required(true)
                                .index(1),
                        )
                        .arg(
                            Arg::new("path")
                                .help("Path for the location, e.g. /api")
                                .required(true)
                                .index(2),
                        )
                        .arg(
                            Arg::new("target_type")
                                .help("Target type: 'instance', 'inst', or 'url'")
                                .required(true)
                                .index(3),
                        )
                        .arg(
                            Arg::new("target_value")
                                .help("Target value: group name for instance, or URL for url type")
                                .required(false)
                                .index(4),
                        )
                        .arg(
                            Arg::new("override_404")
                                .long("override-404")
                                .help("Custom 404 override path")
                                .required(false),
                        ),
                )
                .subcommand(
                    Command::new("delete")
                        .alias("rm")
                        .about("Delete a location from a service")
                        .arg(
                            Arg::new("service_id")
                                .help("Service UUID or name")
                                .required(true)
                                .index(1),
                        )
                        .arg(
                            Arg::new("path")
                                .help("Path of the location to delete")
                                .required(true)
                                .index(2),
                        ),
                ),
        )
}

pub async fn handle(
    config: &mut CliConfig,
    http_client: &Client,
    instance_matches: &clap::ArgMatches,
) -> Result<()> {
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
        Some(("location", location_matches)) => match location_matches.subcommand() {
            Some(("list", args)) => location::list_locations(&http_client, config, args).await,
            Some(("add", args)) => location::add_location(&http_client, config, args).await,
            Some(("delete", args)) => location::delete_location(&http_client, config, args).await,
            Some((_, _)) => {
                eprintln!("Unknown location command");
                Ok(())
            }
            None => {
                // Default to listing locations when no subcommand is provided
                location::list_locations(&http_client, config, location_matches).await
            }
        },
        Some(("new", args)) => {
            let name = args.get_one::<String>("name").unwrap();
            let host = args.get_one::<String>("host").unwrap();
            let allow_http = args.get_flag("allow_http");

            let (host, _) =
                as_domain(host).map_err(|e| anyhow::anyhow!("Invalid host format: {}", e))?;

            // Create default configuration with a single "/" location pointing to default instance group
            let configuration = new::HTTPServiceConfig {
                locations: vec![new::HTTPLocation {
                    path: "/".to_string(),
                    override_404: None,
                    target: new::HTTPLocationTarget::Instance {
                        group: "default".to_string(),
                    },
                }],
                allow_http,
            };

            let request = new::ServiceProvisionRequest {
                region: "dev".to_string(),
                name: name.to_string(),
                host: host.to_string(),
                configuration,
                instance_targets: vec![],
            };

            let response = new::new_service(request, &http_client, config).await?;
            println!(
                "Service created with ID: {} \nHost: https://{}",
                response.service_id, host
            );
            println!(
                "\nUse 'unisrv srv target add {}' to add instance targets",
                response.service_id
            );
            println!(
                "Use 'unisrv srv location add {}' to configure routing",
                response.service_id
            );

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

fn as_domain(host: &str) -> Result<(String, String)> {
    if host.contains('.') {
        return Ok((host.to_owned(), host.to_string()));
    }

    Ok((format!("{}.unisrv.dev", host), host.to_string()))
}
