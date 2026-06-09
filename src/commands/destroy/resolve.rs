//! Resolve which environment `unisrv destroy` targets.
//!
//! Like `up`'s resolver but it never creates: destroy only ever acts on an
//! existing environment. Rules within the manifest's project:
//!   * `--env <name>` pins by name; an unknown name resolves to `None` (nothing
//!     to destroy) so a rerun after a completed destroy is a clean no-op.
//!   * no flag: 0 matches → `None` (no-op), 1 → use it, 2+ → error asking for `--env`.

use anyhow::{Result, bail};
use unisrv_api::ApiClient;
use unisrv_api::models::EnvironmentListEntry;

use crate::commands::up::plan::ResolvedEnvironment;

/// Returns the environment to destroy, or `None` when there is nothing to destroy.
pub async fn resolve_for_destroy(
    client: &dyn ApiClient,
    project: &str,
    env_flag: Option<&str>,
) -> Result<Option<ResolvedEnvironment>> {
    let envs = client.list_environments().await?;
    let matching: Vec<EnvironmentListEntry> = envs
        .environments
        .into_iter()
        .filter(|e| e.project == project)
        .collect();

    if let Some(name) = env_flag {
        // Unknown name → nothing to destroy (keeps reruns idempotent).
        return Ok(matching
            .iter()
            .find(|e| e.name == name)
            .map(ResolvedEnvironment::from));
    }

    match matching.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.into())),
        many => {
            let names: Vec<&str> = many.iter().map(|e| e.name.as_str()).collect();
            bail!(
                "multiple environments exist for project {project:?}: [{}]. Pass --env <name> to pick one.",
                names.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use unisrv_api::models::EnvironmentListResponse;
    use unisrv_api::test_support::MockApiClient;
    use uuid::Uuid;

    fn entry(name: &str, project: &str) -> EnvironmentListEntry {
        EnvironmentListEntry {
            id: Uuid::new_v4(),
            project: project.to_string(),
            name: name.to_string(),
            slug: format!("{name}-slug"),
            display_name: None,
            description: None,
            instance_count: 0,
            service_count: 0,
            deployment_count: 0,
            network_count: 0,
            created_at: NaiveDateTime::default(),
        }
    }

    fn client(entries: Vec<EnvironmentListEntry>) -> MockApiClient {
        MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
            environments: entries,
        }))
    }

    #[tokio::test]
    async fn no_match_returns_none() {
        let c = client(vec![entry("prod", "other")]);
        let got = resolve_for_destroy(&c, "demo", None).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn single_match_returns_env() {
        let c = client(vec![entry("prod", "demo")]);
        let got = resolve_for_destroy(&c, "demo", None).await.unwrap();
        assert_eq!(got.unwrap().name, "prod");
    }

    #[tokio::test]
    async fn multiple_without_flag_errors() {
        let c = client(vec![entry("prod", "demo"), entry("staging", "demo")]);
        let err = resolve_for_destroy(&c, "demo", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("--env"), "{msg}");
    }

    #[tokio::test]
    async fn env_flag_selects_named() {
        let c = client(vec![entry("prod", "demo"), entry("staging", "demo")]);
        let got = resolve_for_destroy(&c, "demo", Some("staging"))
            .await
            .unwrap();
        assert_eq!(got.unwrap().name, "staging");
    }

    #[tokio::test]
    async fn env_flag_unknown_name_returns_none() {
        let c = client(vec![entry("prod", "demo")]);
        let got = resolve_for_destroy(&c, "demo", Some("ghost"))
            .await
            .unwrap();
        assert!(got.is_none(), "unknown --env is a no-op, not a create");
    }
}
