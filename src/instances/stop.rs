use crate::{config::CliConfig, default_spinner, error};
use anyhow::Result;
use reqwest::Client;
use uuid::Uuid;

pub async fn stop_instance(
    client: &Client,
    config: &mut CliConfig,
    uuid: Uuid,
    timeout_ms: u32,
) -> Result<()> {
    let spinner = default_spinner();
    spinner.set_prefix("Stopping instance...");
    spinner.set_message("Attempting graceful shutdown...");
    let spinner_clone = spinner.clone();
    let start_time = std::time::Instant::now();
    let progress_task = tokio::spawn(async move {
        loop {
            if spinner.is_finished() {
                return;
            }
            let elapsed = start_time.elapsed().as_secs_f32();
            let total_time = timeout_ms as f32 / 1000.0;
            if elapsed >= total_time {
                spinner.set_message("Stopping instance forcefully.");
                return;
            }
            spinner.set_message(format!(
                "Attempting graceful shutdown... ({elapsed:.1}/{total_time:.1} s)",
            ));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });

    let response = client
        .delete(config.url(&format!("/instance/{uuid}")))
        .bearer_auth(config.token(client).await?)
        .json(&serde_json::json!({
            "timeout_ms": timeout_ms,
        }))
        .send()
        .await?;

    spinner_clone.finish_and_clear();
    let _ = progress_task.await;

    if response.status().is_success() {
        println!("Successfully stopped instance with UUID: {uuid}");
        Ok(())
    } else {
        error::handle_http_error(response, "stop instance").await
    }
}
