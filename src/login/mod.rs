use chrono::DateTime;
use clap::Command;
use console::Emoji;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yapp::PasswordReader;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub user_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<chrono::Utc>,
    pub refresh_session_id: Uuid,
    pub refresh_token: String,
    pub refresh_expires_at: DateTime<chrono::Utc>,
}

use crate::config::CliConfig;
use crate::error;
use anyhow::Result;

pub fn command() -> Command {
    Command::new("login")
        .about("Login with a user account")
        .arg_required_else_help(true)
        .arg(
            clap::Arg::new("username")
                .short('u')
                .long("username")
                .help("Username to login with")
                .required(true),
        )
        .arg(
            clap::Arg::new("password")
                .short('p')
                .long("password")
                .value_name("PASSWORD")
                .help("Optional password to login with. NOTE: RECOMMEND TO LEAVE EMPTY AND USE PROMPT")
                .required(false),
        )
}

pub async fn handle(
    config: &mut CliConfig,
    http_client: &Client,
    instance_matches: &clap::ArgMatches,
) -> Result<()> {
    let username = instance_matches
        .get_one::<String>("username")
        .expect("Username is required");
    let password = match instance_matches.get_one::<String>("password") {
        Some(p) => p.clone(),
        None => {
            let mut yapp = yapp::Yapp::new().with_echo_symbol('*');
            yapp.read_password_with_prompt("Enter password: ")
                .map_err(|e| anyhow::anyhow!("Failed to read password from stdin: {}", e))?
        }
    };
    let response: reqwest::Response = http_client
        .post(config.url("/auth/login/basic"))
        .basic_auth(username, Some(password))
        .send()
        .await?;
    if !response.status().is_success() {
        return error::handle_http_error(response, "login").await;
    }
    let login_response: LoginResponse = response.json().await?;
    config.save_auth_from_login(login_response)?;
    eprintln!(
        "{} Successfully logged in as user: {}",
        Emoji("🔒", ""),
        username
    );

    Ok(())
}
