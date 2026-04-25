mod commands;

use clap::{Parser, Subcommand};
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
    };

    if let Err(err) = result {
        match err.downcast_ref::<ApiError>() {
            Some(ApiError::AuthRequired(msg)) => eprintln!("Error: {msg}"),
            Some(ApiError::Server { status, reason }) => eprintln!("Error ({status}): {reason}"),
            _ => eprintln!("Error: {err}"),
        }
        std::process::exit(1);
    }
}
