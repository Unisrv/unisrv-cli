//! `unisrv instance logs <ref>` — print or follow an instance's logs.
//!
//! Log frames are routed by type so the output pipes cleanly: application
//! stdout goes to our stdout verbatim, application stderr to our stderr, and
//! platform `system`/`state` frames to stderr (dimmed, timestamped). That way
//! `unisrv instance logs web | grep ...` sees only the program's stdout.

use anyhow::Result;
use unisrv_api::ApiClient;
use unisrv_api::models::LogMessage;
use uuid::Uuid;

use super::resolve::resolve_instance;
use crate::commands::up::plan::ResolvedEnvironment;

/// Print or follow the logs of the instance referenced by `reference` within
/// `env`. Without `follow`, prints the current log history and returns. With
/// `follow`, streams until the server closes the connection or errors.
pub async fn logs(
    client: &dyn ApiClient,
    env: &ResolvedEnvironment,
    reference: &str,
    follow: bool,
) -> Result<()> {
    let instances = client.list_instances(env.id).await?;
    let instance_id = resolve_instance(reference, &instances.instances)?.id;

    if follow {
        follow_logs(client, env.id, instance_id).await
    } else {
        let history = client.get_instance_logs(env.id, instance_id).await?;
        for msg in &history {
            emit(route(msg));
        }
        Ok(())
    }
}

/// Stream until the server closes the connection (a normal end, e.g. the
/// instance stopped) or a transport error occurs. A clean close is success.
async fn follow_logs(client: &dyn ApiClient, env_id: Uuid, instance_id: Uuid) -> Result<()> {
    use futures_util::StreamExt;

    let mut stream = client.stream_instance_logs(env_id, instance_id).await?;
    while let Some(frame) = stream.next().await {
        emit(route(&frame?));
    }
    eprintln!("{}", console::style("stream closed").dim());
    Ok(())
}

/// Write a routed line to the appropriate stream, dimming platform chatter when
/// stderr is an interactive terminal (no ANSI in pipes).
fn emit(line: Option<RoutedLine>) {
    let Some(line) = line else { return };
    match line.sink {
        Sink::Out => println!("{}", line.text),
        Sink::Err if line.dim && console::user_attended_stderr() => {
            eprintln!("{}", console::style(line.text).dim());
        }
        Sink::Err => eprintln!("{}", line.text),
    }
}

/// Which of our output streams a routed log line is written to.
#[derive(Debug, PartialEq, Eq)]
enum Sink {
    Out,
    Err,
}

/// A log frame routed to a stream, with the text to print and whether it should
/// be dimmed (platform chatter, not application output).
#[derive(Debug, PartialEq, Eq)]
struct RoutedLine {
    sink: Sink,
    text: String,
    dim: bool,
}

/// Decide where a log frame goes and how it reads. Returns `None` for frames
/// that carry nothing to show. Pure, so routing is testable without a terminal.
fn route(msg: &LogMessage) -> Option<RoutedLine> {
    match msg.log_type.as_str() {
        // Application output is forwarded verbatim, including a genuinely blank
        // line (`Some("")`). A frame carrying no `message` field at all has
        // nothing to show, so it's dropped rather than printed as an empty line.
        "stdout" => msg.message.as_ref().map(|text| RoutedLine {
            sink: Sink::Out,
            text: text.clone(),
            dim: false,
        }),
        "stderr" => msg.message.as_ref().map(|text| RoutedLine {
            sink: Sink::Err,
            text: text.clone(),
            dim: false,
        }),
        // Platform chatter is only worth a timestamped line when it carries a
        // message; an empty `system` frame is noise, not a blank "[ts] " line.
        "system" => non_empty_chatter(msg),
        "state" => {
            let state = msg.state.clone().unwrap_or_default();
            if state.is_empty() {
                return None;
            }
            Some(RoutedLine {
                sink: Sink::Err,
                text: format!("[{}] state: {state}", fmt_ts(msg.timestamp_ms)),
                dim: true,
            })
        }
        // An unrecognised frame type still shouldn't be dropped silently: show
        // any message it carries on stderr, dimmed, rather than on stdout.
        _ => non_empty_chatter(msg),
    }
}

/// A dimmed, timestamped stderr line for a platform frame — unless it has no
/// message to carry, in which case there's nothing to show.
fn non_empty_chatter(msg: &LogMessage) -> Option<RoutedLine> {
    let body = msg.message.as_deref().unwrap_or_default();
    if body.is_empty() {
        return None;
    }
    Some(RoutedLine {
        sink: Sink::Err,
        text: format!("[{}] {body}", fmt_ts(msg.timestamp_ms)),
        dim: true,
    })
}

/// Format an epoch-millisecond timestamp as a readable UTC time. Falls back to
/// the raw number if it's out of range.
fn fmt_ts(timestamp_ms: u64) -> String {
    let secs = (timestamp_ms / 1000) as i64;
    let nanos = ((timestamp_ms % 1000) * 1_000_000) as u32;
    match chrono::DateTime::from_timestamp(secs, nanos) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => timestamp_ms.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use unisrv_api::ApiError;
    use unisrv_api::models::{InstanceListEntry, InstanceListResponse, InstanceState};
    use unisrv_api::test_support::MockApiClient;

    fn msg(log_type: &str, message: Option<&str>, state: Option<&str>) -> LogMessage {
        LogMessage {
            log_type: log_type.to_string(),
            timestamp_ms: 1_700_000_000_000,
            state: state.map(String::from),
            message: message.map(String::from),
        }
    }

    fn env() -> ResolvedEnvironment {
        ResolvedEnvironment {
            id: Uuid::new_v4(),
            name: "prod".to_string(),
            project: "demo".to_string(),
            slug: "ab12".to_string(),
        }
    }

    fn instance(id: Uuid, name: &str) -> InstanceListEntry {
        InstanceListEntry {
            id,
            name: Some(name.to_string()),
            state: InstanceState("running".to_string()),
            container_image: "nginx:latest".to_string(),
            created_at: chrono::NaiveDateTime::default(),
            deployment: None,
        }
    }

    fn list_of(instances: Vec<InstanceListEntry>) -> InstanceListResponse {
        InstanceListResponse { instances }
    }

    #[test]
    fn stdout_frames_go_to_stdout_verbatim() {
        let routed = route(&msg("stdout", Some("hello world"), None)).unwrap();
        assert_eq!(routed.sink, Sink::Out);
        assert_eq!(routed.text, "hello world");
        assert!(!routed.dim, "application output is not dimmed");
    }

    #[test]
    fn stderr_frames_go_to_stderr_verbatim() {
        let routed = route(&msg("stderr", Some("oops"), None)).unwrap();
        assert_eq!(routed.sink, Sink::Err);
        assert_eq!(routed.text, "oops");
        assert!(!routed.dim);
    }

    #[test]
    fn system_frames_are_dimmed_on_stderr_and_keep_their_message() {
        let routed = route(&msg("system", Some("pulling image"), None)).unwrap();
        assert_eq!(routed.sink, Sink::Err);
        assert!(routed.dim, "platform chatter is dimmed");
        assert!(routed.text.contains("pulling image"));
    }

    #[test]
    fn state_frames_surface_the_state_on_stderr() {
        let routed = route(&msg("state", None, Some("online"))).unwrap();
        assert_eq!(routed.sink, Sink::Err);
        assert!(routed.dim);
        assert!(routed.text.contains("online"));
    }

    #[test]
    fn state_frame_without_a_state_is_dropped() {
        assert!(route(&msg("state", None, None)).is_none());
    }

    #[test]
    fn blank_stdout_line_is_preserved_verbatim() {
        // A program that prints an empty line is real output; keep it.
        let routed = route(&msg("stdout", Some(""), None)).unwrap();
        assert_eq!(routed.sink, Sink::Out);
        assert_eq!(routed.text, "");
    }

    #[test]
    fn stdout_frame_without_a_message_is_dropped() {
        // No `message` field at all = nothing to print, not a blank line.
        assert!(route(&msg("stdout", None, None)).is_none());
    }

    #[test]
    fn empty_system_frame_is_dropped_not_a_bare_timestamp() {
        assert!(route(&msg("system", None, None)).is_none());
        assert!(route(&msg("system", Some(""), None)).is_none());
    }

    #[tokio::test]
    async fn non_follow_resolves_ref_and_fetches_that_instances_logs() {
        let env = env();
        let id = Uuid::new_v4();
        let mock = MockApiClient::logged_in()
            .with_list_instances(Ok(list_of(vec![instance(id, "web")])))
            .push_instance_logs(Ok(vec![msg("stdout", Some("hi"), None)]));

        let result = logs(&mock, &env, "web", false).await;

        assert!(result.is_ok(), "expected ok, got {result:?}");
        assert_eq!(
            mock.calls.lock().unwrap().get_instance_logs_calls,
            vec![(env.id, id)]
        );
    }

    #[tokio::test]
    async fn unknown_ref_errors_before_fetching_logs() {
        let mock = MockApiClient::logged_in()
            .with_list_instances(Ok(list_of(vec![instance(Uuid::new_v4(), "web")])));

        let err = logs(&mock, &env(), "ghost", false).await.unwrap_err();

        assert!(format!("{err:#}").contains("ghost"));
        assert!(
            mock.calls
                .lock()
                .unwrap()
                .get_instance_logs_calls
                .is_empty(),
            "should not fetch logs for an unresolved ref"
        );
    }

    #[tokio::test]
    async fn follow_drains_the_stream_until_close_and_succeeds() {
        let env = env();
        let id = Uuid::new_v4();
        let mock = MockApiClient::logged_in()
            .with_list_instances(Ok(list_of(vec![instance(id, "web")])))
            .push_stream_logs(vec![
                msg("system", Some("starting"), None),
                msg("stdout", Some("ready"), None),
            ]);

        let result = logs(&mock, &env, "web", true).await;

        assert!(
            result.is_ok(),
            "clean stream close is success, got {result:?}"
        );
        assert_eq!(
            mock.calls.lock().unwrap().stream_instance_logs_calls,
            vec![(env.id, id)]
        );
    }

    #[tokio::test]
    async fn follow_surfaces_a_connect_error() {
        // The upgrade itself failing (e.g. 404 on the WS endpoint) must error,
        // not be silently reported as a clean "stream closed".
        let id = Uuid::new_v4();
        let mock = MockApiClient::logged_in()
            .with_list_instances(Ok(list_of(vec![instance(id, "web")])))
            .push_stream_connect_error(ApiError::Server {
                status: 404,
                reason: "instance not found".into(),
            });

        let err = logs(&mock, &env(), "web", true).await.unwrap_err();
        assert!(format!("{err:#}").contains("instance not found"), "{err:#}");
    }

    #[tokio::test]
    async fn follow_propagates_a_mid_stream_transport_error() {
        let id = Uuid::new_v4();
        let mock = MockApiClient::logged_in()
            .with_list_instances(Ok(list_of(vec![instance(id, "web")])))
            .push_stream_logs_frames(vec![
                Ok(msg("stdout", Some("line"), None)),
                Err(ApiError::Other(anyhow::anyhow!("connection reset"))),
            ]);

        let err = logs(&mock, &env(), "web", true).await.unwrap_err();
        assert!(format!("{err:#}").contains("connection reset"));
    }
}
