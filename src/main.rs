mod commands;

use clap::{Parser, Subcommand};
use commands::up::parse_error::ConfigParseError;
use unisrv_api::{ApiClient, ApiError, HttpApiClient};

#[derive(Parser)]
#[command(
    name = "unisrv",
    about = "Declarative infrastructure deployments on Unisrv"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Login with a user account
    Login {
        /// Username
        #[arg(short, long)]
        username: Option<String>,
        /// Password (insecure — prefer interactive prompt)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Authentication utilities
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    /// Manage service hosts (domains)
    Host {
        #[command(subcommand)]
        command: HostCommands,
    },
    /// Manage container registry credentials
    #[command(alias = "reg")]
    Registry {
        #[command(subcommand)]
        command: RegistryCommands,
    },
    /// Apply the unisrv.hcl in the current directory
    Up {
        /// Pin which environment to target by name (overrides project lookup)
        #[arg(long)]
        env: Option<String>,
    },
    /// Destroy the selected environment: delete all its services, deployments,
    /// standalone instances, and the environment itself
    Destroy {
        /// Pin which environment to destroy by name (overrides project lookup)
        #[arg(long)]
        env: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Print a valid access token to stdout
    Token {
        /// Output as JSON with expiry information
        #[arg(short, long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum HostCommands {
    /// Claim a host (domain) and provision a TLS certificate
    Claim {
        /// Hostname to claim, e.g. example.com
        hostname: String,
    },
    /// List claimed hosts
    #[command(alias = "ls")]
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum RegistryCommands {
    /// Add a container registry credential
    Add {
        /// Registry hostname, e.g. ghcr.io
        hostname: String,
        /// Registry username
        #[arg(short, long)]
        username: Option<String>,
        /// Read password from stdin instead of prompting interactively
        #[arg(long)]
        password_stdin: bool,
        /// Skip validating credentials against the upstream registry
        #[arg(long)]
        no_validate: bool,
    },
    /// List configured registries
    #[command(alias = "ls")]
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Update credentials for a registry
    Update {
        /// Registry hostname
        hostname: String,
        /// New username
        #[arg(short, long)]
        username: Option<String>,
        /// Read a new password from stdin
        #[arg(long)]
        password_stdin: bool,
        /// Skip validating credentials against the upstream registry
        #[arg(long)]
        no_validate: bool,
    },
    /// Delete a registry credential
    #[command(alias = "rm")]
    Delete {
        /// Registry hostname
        hostname: String,
        /// Skip the confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Test that stored credentials still work against the upstream registry
    Test {
        /// Registry hostname
        hostname: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();
    let client = HttpApiClient::from_env();

    let client: &dyn ApiClient = &client;
    let result = match cli.command {
        Commands::Login { username, password } => {
            commands::login::run(client, username.as_deref(), password.as_deref()).await
        }
        Commands::Auth { command } => match command {
            AuthCommands::Token { json } => commands::auth::token(client, json).await,
        },
        Commands::Host { command } => match command {
            HostCommands::Claim { hostname } => commands::host::claim(client, &hostname).await,
            HostCommands::List { json } => commands::host::list(client, json).await,
        },
        Commands::Registry { command } => match command {
            RegistryCommands::Add {
                hostname,
                username,
                password_stdin,
                no_validate,
            } => {
                commands::registry::add(
                    client,
                    &hostname,
                    username.as_deref(),
                    password_stdin,
                    !no_validate,
                )
                .await
            }
            RegistryCommands::List { json } => commands::registry::list(client, json).await,
            RegistryCommands::Update {
                hostname,
                username,
                password_stdin,
                no_validate,
            } => {
                commands::registry::update(
                    client,
                    &hostname,
                    username.as_deref(),
                    password_stdin,
                    !no_validate,
                )
                .await
            }
            RegistryCommands::Delete { hostname, yes } => {
                commands::registry::delete(client, &hostname, yes).await
            }
            RegistryCommands::Test { hostname } => {
                commands::registry::test(client, &hostname).await
            }
        },
        Commands::Up { env } => commands::up::run(client, env.as_deref()).await,
        Commands::Destroy { env } => commands::destroy::run(client, env.as_deref()).await,
    };

    if let Err(err) = result {
        if let Some(parse_err) = err.downcast_ref::<ConfigParseError>() {
            eprint!("{parse_err}");
        } else if let Some(ApiError::AuthRequired(msg)) = err.downcast_ref::<ApiError>() {
            eprintln!("Error: {msg}");
        } else if let Some(ApiError::Server { status, reason }) = err.downcast_ref::<ApiError>() {
            eprintln!("Error ({status}): {reason}");
        } else {
            eprintln!("Error: {err:#}");
        }
        std::process::exit(1);
    }
}
