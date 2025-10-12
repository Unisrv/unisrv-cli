use crate::config::CliConfig;
use crate::registry::client;
use anyhow::Result;
use console::style;
use std::io::{self, Read};

pub async fn login_registry(config: &mut CliConfig, args: &clap::ArgMatches) -> Result<()> {
    let registry = args
        .get_one::<String>("registry")
        .expect("Registry is required");

    let username = args.get_one::<String>("username").cloned();

    let password = if args.get_flag("password_stdin") {
        // Read password from stdin
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|e| anyhow::anyhow!("Failed to read password from stdin: {}", e))?;
        Some(buffer.trim().to_string())
    } else {
        args.get_one::<String>("password").cloned()
    };

    // Perform the actual login
    let token_response = client::login_registry(
        registry,
        username.as_deref(),
        password.as_deref(),
    )
    .await?;

    // Display the results
    println!("{}", style("Registry Login Successful").green().bold());
    println!("Registry: {}", style(registry).cyan());

    if let Some(ref user) = username {
        println!("Username: {}", style(user).cyan());
    }

    // Check which token field is present (different registries use different fields)
    let token = token_response
        .token
        .clone()
        .or(token_response.access_token.clone());

    if let Some(ref token_str) = token {
        println!(
            "\nToken (first 50 chars): {}",
            style(&token_str[..token_str.len().min(50)]).dim()
        );
        if token_str.len() > 50 {
            println!("{}", style("... (truncated)").dim());
        }
    } else {
        println!(
            "\n{}",
            style("No authentication required (anonymous access)").yellow()
        );
    }

    let token_expiry = token_response.expires_in.map(|expires_in| {
        chrono::Utc::now() + chrono::Duration::try_seconds(expires_in).unwrap()
    });

    if let Some(expires_in) = token_response.expires_in {
        println!(
            "\nExpires in: {} seconds ({} minutes)",
            style(expires_in).cyan(),
            style(expires_in / 60).cyan()
        );
    }

    // Store credentials in config
    match config.save_registry_auth(registry, username, password, token, token_expiry) {
        Ok(_) => {
            println!(
                "\n{}",
                style("✓ Credentials saved successfully").green()
            );
        }
        Err(e) => {
            let program = std::env::args().nth(0).unwrap_or_else(|| "unisrv".to_string());
            eprintln!(
                "\n{} {}",
                style("Warning:").yellow().bold(),
                style(format!("Failed to save credentials: {}", e)).yellow()
            );
            eprintln!(
                "  {}",
                style(format!("You may need to login to unisrv first with: {} login", program))
                    .dim()
                    .italic()
            );
        }
    }

    Ok(())
}
