use ::serde::Deserialize;
use console::Emoji;

use crate::config::CliConfig;
use anyhow::Result;
use futures_util::TryStreamExt;
use indicatif::ProgressBar;
use reqwest::Client;
use reqwest_websocket::RequestBuilderExt;
use uuid::Uuid;

static ROCKET: Emoji = Emoji("🚀 ", "");
static CHECK: Emoji = Emoji("✅ ", "");
static CRANE: Emoji = Emoji("🏗️ ", "");

pub async fn stream_logs(
    client: &Client,
    config: &mut CliConfig,
    uuid: Uuid,
    mut progress: Option<ProgressBar>,
) -> Result<()> {
    let response = client
        .get(&config.ws_url(&format!("/instance/{}/logs/stream", uuid)))
        .bearer_auth(config.token(client).await?)
        .upgrade()
        .send()
        .await?;

    let mut ws = response
        .into_websocket()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to upgrade to WebSocket: {}", e))?;

    loop {
        let message = ws.try_next().await;
        match message {
            Ok(Some(reqwest_websocket::Message::Text(text))) => {
                let log_message: InstanceLogMessage = serde_json::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("Failed to parse log message: {}", e))?;
                if handle_log_message(log_message, progress.as_mut()) {
                    progress = None; // Instance is ready, no need for progress bar anymore
                }
            }
            _ => break, // Error occurred
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceLogType {
    State,
    System,
    Stdout,
    Stderr,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmInitState {
    Online,
    PullingContainerImage,
    ExecutingContainer,
}

#[derive(Debug, Deserialize)]
struct InstanceLogMessage {
    log_type: InstanceLogType,
    timestamp_ms: u64,
    message: Option<String>,
    state: Option<VmInitState>,
}

impl InstanceLogMessage {
    pub fn datetime(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(
            (self.timestamp_ms / 1000) as i64,
            ((self.timestamp_ms % 1000) * 1_000_000) as u32,
        )
        .unwrap()
    }
}

fn handle_log_message(message: InstanceLogMessage, progress: Option<&mut ProgressBar>) -> bool {
    match message.log_type {
        InstanceLogType::System => {
            if let Some(pb) = progress {
                pb.set_message(message.message.unwrap_or_default());
            } else {
                eprintln!(
                    "[Instance] {} - {}",
                    message.datetime().format("%Y-%m-%d %H:%M:%S"),
                    message.message.unwrap_or_default()
                );
            }
        }
        InstanceLogType::Stdout => println!("{}", message.message.unwrap_or_default()),
        InstanceLogType::Stderr => eprintln!("{}", message.message.unwrap_or_default()),
        InstanceLogType::State => match message.state.expect("State without state?") {
            VmInitState::Online => {
                if let Some(pb) = progress {
                    pb.set_prefix(format!("{}Instance is online", ROCKET));
                } else {
                    eprintln!("Instance is online");
                }
            }
            VmInitState::PullingContainerImage => {
                if let Some(pb) = progress {
                    pb.set_prefix(format!("{}Pulling container image", CRANE));
                } else {
                    eprintln!("Pulling container image...");
                }
            }
            VmInitState::ExecutingContainer => {
                if let Some(pb) = progress {
                    pb.set_prefix(format!("{}Executing container", CHECK));
                    pb.finish_and_clear();
                } else {
                    eprintln!("Executing container...");
                }
                return true; // Indicate that the instance is ready
            }
        },
    };
    false
}
