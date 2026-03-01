use anyhow::Result;
use console::Emoji;
use dialoguer::Confirm;
use reqwest::Client;

use crate::{config::CliConfig, default_spinner, error};

use super::list::HostResponse;

static HOST: Emoji = Emoji("🌐 ", "");
static LOCK: Emoji = Emoji("🔒 ", "");
static CHECK: Emoji = Emoji("✅ ", "");

const IPV4: &str = "70.34.214.14";
const IPV6: &str = "2a05:f480:2000:16fd::1";
const EDGE_HOST: &str = "srvedge.net";

/// Entry point from `unisrv host cert <host>`
pub async fn request_cert(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let host_input = args.get_one::<String>("host").unwrap();

    let progress = default_spinner();
    progress.set_prefix("Resolving host...");

    let hosts = super::list::list(client, config).await?;
    let resolved_id = super::resolve_host_id(host_input, &hosts)?;

    let host = hosts
        .iter()
        .find(|h| h.id == resolved_id)
        .ok_or_else(|| anyhow::anyhow!("Host not found"))?;

    progress.finish_and_clear();

    request_cert_for_host(client, config, host).await
}

/// Shared cert wizard, callable from both `cert` and `claim` commands.
pub async fn request_cert_for_host(
    client: &Client,
    config: &mut CliConfig,
    host: &HostResponse,
) -> Result<()> {
    let domain = &host.host;
    let resolved_id = host.id;
    let is_subdomain = domain.matches('.').count() > 1;

    // Check if already has a certificate
    if host.certificate_type.is_some() {
        println!(
            "{}{} {} already has a certificate (type: {})",
            LOCK,
            HOST,
            console::style(domain).bold().cyan(),
            console::style(host.certificate_type.as_deref().unwrap_or("unknown")).green()
        );

        let reissue = Confirm::new()
            .with_prompt("Request a new certificate anyway?")
            .default(false)
            .interact()?;

        if !reissue {
            return Ok(());
        }
    }

    let is_managed_domain = domain.ends_with(".unisrv.dev");

    if !is_managed_domain {
        // DNS wizard for custom domains
        println!(
            "{}{} TLS Certificate Setup for {}",
            LOCK,
            HOST,
            console::style(domain).bold().cyan()
        );
        println!();
        println!("  Before provisioning a certificate, point your DNS to our edge servers:");
        println!();

        if is_subdomain {
            println!(
                "  {} Create a {} record pointing to {}",
                console::style("(recommended)").dim(),
                console::style("CNAME").bold(),
                console::style(EDGE_HOST).bold().green(),
            );
            println!();
            println!("  Or, set explicit address records:");
            println!(
                "    {}    {}  {}  {}",
                console::style("A").bold(),
                console::style(domain).cyan(),
                console::style("->").dim(),
                console::style(IPV4).green()
            );
            println!(
                "    {} {}  {}  {}",
                console::style("AAAA").bold(),
                console::style(domain).cyan(),
                console::style("->").dim(),
                console::style(IPV6).green()
            );
        } else {
            println!(
                "  {} If your DNS provider supports it (e.g. Cloudflare), create an",
                console::style("(recommended)").dim(),
            );
            println!(
                "  {} record pointing to {}",
                console::style("ALIAS / CNAME flattening").bold(),
                console::style(EDGE_HOST).bold().green()
            );
            println!();
            println!("  Otherwise, set address records:");
            println!(
                "    {}    {}  {}  {}",
                console::style("A").bold(),
                console::style(domain).cyan(),
                console::style("->").dim(),
                console::style(IPV4).green()
            );
            println!(
                "    {} {}  {}  {}",
                console::style("AAAA").bold(),
                console::style(domain).cyan(),
                console::style("->").dim(),
                console::style(IPV6).green()
            );
        }

        println!();

        let confirm = Confirm::new()
            .with_prompt("Are DNS records configured. Proceed with certificate request?")
            .default(true)
            .interact()?;

        if !confirm {
            println!("Certificate request cancelled.");
            return Ok(());
        }
    }

    // Request the certificate
    let progress = default_spinner();
    progress.set_prefix("Requesting certificate");
    progress.set_message(format!(
        "{LOCK} Provisioning TLS certificate for {}... (this may take a moment)",
        console::style(domain).cyan()
    ));

    let response = client
        .post(config.url(&format!("/hosts/{resolved_id}/cert")))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let updated_host: HostResponse = response.json().await?;
        println!(
            "{}{} TLS certificate provisioned for {}",
            CHECK,
            LOCK,
            console::style(&updated_host.host).bold().green()
        );
        if let Some(cert_type) = &updated_host.certificate_type {
            println!("  Certificate type: {}", console::style(cert_type).cyan());
        }
        if let Some(valid_until) = &updated_host.certificate_valid_until {
            println!("  Valid until:      {}", console::style(valid_until).cyan());
        }
    } else {
        let status = response.status();
        error::handle_http_error(response, "request certificate").await.map_err(|e| {
            let mut msg = format!("{e}");
            msg.push_str(&format!(
                "\n\n  {} Verify that DNS is pointing to our edge servers:",
                console::style("Hint:").bold().yellow()
            ));
            if is_subdomain {
                msg.push_str(&format!(
                    "\n    CNAME  {} -> {EDGE_HOST}",
                    domain
                ));
            } else {
                msg.push_str(&format!(
                    "\n    A      {} -> {IPV4}",
                    domain
                ));
            }
            msg.push_str(&format!(
                "\n    AAAA   {} -> {IPV6}",
                domain
            ));
            if status.as_u16() == 400 {
                msg.push_str("\n\n  DNS changes can take time to propagate depending on your TTL settings.");
                msg.push_str("\n  If you just updated your records, wait a few minutes and try again.");
            }
            anyhow::anyhow!("{msg}")
        })?;
    }

    Ok(())
}
