use ::serde::Deserialize;
use console::Emoji;
use std::collections::VecDeque;
use std::time::Duration;

use crate::config::CliConfig;
use anyhow::Result;
use futures_util::TryStreamExt;
use indicatif::ProgressBar;
use reqwest::Client;
use reqwest_websocket::RequestBuilderExt;
use tokio::time::Instant;
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
        .get(config.ws_url(&format!("/instance/{uuid}/logs/stream")))
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
        .unwrap_or_default()
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
        InstanceLogType::State => match message.state {
            None => return false,
            Some(state) => match state {
                VmInitState::Online => {
                    if let Some(pb) = progress {
                        pb.set_prefix(format!("{ROCKET}Instance is online"));
                    } else {
                        eprintln!("Instance is online");
                    }
                }
                VmInitState::PullingContainerImage => {
                    if let Some(pb) = progress {
                        pb.set_prefix(format!("{CRANE}Pulling container image"));
                    } else {
                        eprintln!("Pulling container image...");
                    }
                }
                VmInitState::ExecutingContainer => {
                    if let Some(pb) = progress {
                        pb.set_prefix(format!("{CHECK}Executing container"));
                        pb.finish_and_clear();
                    } else {
                        eprintln!("Executing container...");
                    }
                    return true;
                }
            },
        },
    };
    false
}

/// Stream WebSocket logs for an instance until it reaches ExecutingContainer state,
/// then wait `healthy_wait` duration for early crash detection before returning Ok.
pub async fn stream_logs_until_running(
    client: &Client,
    config: &mut CliConfig,
    uuid: Uuid,
    progress: Option<ProgressBar>,
    healthy_wait: Duration,
) -> Result<()> {
    let response = client
        .get(config.ws_url(&format!("/instance/{uuid}/logs/stream")))
        .bearer_auth(config.token(client).await?)
        .upgrade()
        .send()
        .await?;

    let mut ws = response
        .into_websocket()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to upgrade to WebSocket: {}", e))?;

    let mut log_lines: VecDeque<String> = VecDeque::with_capacity(5);
    let mut health_deadline: Option<Instant> = None;

    loop {
        if let Some(deadline) = health_deadline {
            match tokio::time::timeout_at(deadline, ws.try_next()).await {
                Err(_elapsed) => {
                    // Timeout elapsed — instance survived the health wait period
                    if let Some(pb) = &progress {
                        pb.finish_and_clear();
                    }
                    return Ok(());
                }
                Ok(Ok(None)) | Ok(Err(_)) => {
                    return Err(anyhow::anyhow!(
                        "Instance connection closed unexpectedly during health check"
                    ));
                }
                Ok(Ok(Some(_))) => {
                    // Message received during health wait — continue waiting
                }
            }
        } else {
            match ws.try_next().await {
                Ok(Some(reqwest_websocket::Message::Text(text))) => {
                    let log_message: InstanceLogMessage = serde_json::from_str(&text)
                        .map_err(|e| anyhow::anyhow!("Failed to parse log message: {}", e))?;

                    match log_message.log_type {
                        InstanceLogType::System
                        | InstanceLogType::Stdout
                        | InstanceLogType::Stderr => {
                            if let Some(msg) = log_message.message {
                                if log_lines.len() >= 4 {
                                    log_lines.pop_front();
                                }
                                log_lines.push_back(msg);
                                if let Some(pb) = &progress {
                                    let display: Vec<&str> =
                                        log_lines.iter().map(|s| s.as_str()).collect();
                                    pb.set_message(display.join(" | "));
                                }
                            }
                        }
                        InstanceLogType::State => {
                            let Some(state) = log_message.state else {
                                continue;
                            };
                            match state {
                                VmInitState::ExecutingContainer => {
                                    if let Some(pb) = &progress {
                                        pb.set_prefix(format!("{CHECK}Executing container"));
                                    }
                                    health_deadline = Some(Instant::now() + healthy_wait);
                                }
                                VmInitState::Online => {
                                    if let Some(pb) = &progress {
                                        pb.set_prefix(format!("{ROCKET}Instance is online"));
                                    }
                                }
                                VmInitState::PullingContainerImage => {
                                    if let Some(pb) = &progress {
                                        pb.set_prefix(format!("{CRANE}Pulling container image"));
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(Some(_)) => {} // Non-text WS message, ignore
                Ok(None) | Err(_) => {
                    if let Some(pb) = &progress {
                        pb.finish_and_clear();
                    }
                    return Err(anyhow::anyhow!(
                        "Instance connection closed before reaching running state"
                    ));
                }
            }
        }
    }
}

pub async fn get_logs(client: &Client, config: &mut CliConfig, uuid: Uuid) -> Result<()> {
    let response = client
        .get(config.url(&format!("/instance/{uuid}/logs")))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to get logs: {} - {}",
            response.status(),
            response.text().await?
        ));
    }

    let logs: Vec<InstanceLogMessage> = response.json().await?;

    for log in logs {
        handle_log_message(log, None);
    }

    Ok(())
}
