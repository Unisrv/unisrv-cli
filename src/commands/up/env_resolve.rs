//! Resolve which environment `unisrv up` targets.
//!
//! Rules:
//! 1. `--env <name>` flag, if provided, pins selection by name within the project.
//!    If no env with that name exists for the project, build a Create action
//!    using that name as the default.
//! 2. Otherwise filter `list_environments()` by project:
//!    - 0 matches  → prompt for name/display/desc, build Create action.
//!    - 1 match    → use it.
//!    - 2+ matches → error listing options, ask user to pass --env.
//!
//! The interactive prompt is abstracted behind [`Prompter`] so tests can
//! supply scripted answers.

use anyhow::{Result, bail};
use unisrv_api::ApiClient;
use unisrv_api::models::{CreateEnvironmentRequest, EnvironmentListEntry};

use super::defaults::{DEFAULT_ENV_NAME, default_env_display_name};
use super::plan::{EnvAction, ResolvedEnvironment};

/// Abstraction over user prompting for env metadata. Production uses a
/// dialoguer-backed impl; tests inject scripted answers.
pub trait Prompter {
    /// Prompt for a string with an optional default. Empty answer should yield default.
    fn prompt_string(&self, prompt: &str, default: Option<&str>) -> Result<String>;

    /// Prompt for an optional string. Empty answer → None.
    fn prompt_optional(&self, prompt: &str) -> Result<Option<String>>;
}

pub async fn resolve(
    client: &dyn ApiClient,
    project: &str,
    env_flag: Option<&str>,
    prompter: &dyn Prompter,
) -> Result<EnvAction> {
    let envs = client.list_environments().await?;
    let matching: Vec<EnvironmentListEntry> = envs
        .environments
        .into_iter()
        .filter(|e| e.project == project)
        .collect();

    if let Some(name) = env_flag {
        if let Some(found) = matching.iter().find(|e| e.name == name) {
            return Ok(EnvAction::Use(entry_to_resolved(found)));
        }
        // --env named a non-existent env: prompt for the rest.
        let req = prompt_create_env(prompter, project, name)?;
        return Ok(EnvAction::Create(req));
    }

    match matching.as_slice() {
        [] => {
            let req = prompt_create_env(prompter, project, DEFAULT_ENV_NAME)?;
            Ok(EnvAction::Create(req))
        }
        [only] => Ok(EnvAction::Use(entry_to_resolved(only))),
        many => {
            let names: Vec<&str> = many.iter().map(|e| e.name.as_str()).collect();
            bail!(
                "multiple environments exist for project {project:?}: [{}]. Pass --env <name> to pick one.",
                names.join(", ")
            );
        }
    }
}

fn prompt_create_env(
    prompter: &dyn Prompter,
    project: &str,
    default_name: &str,
) -> Result<CreateEnvironmentRequest> {
    let name = prompter.prompt_string("Environment name", Some(default_name))?;
    let default_display = default_env_display_name(project);
    let display = prompter.prompt_string("Display name", Some(&default_display))?;
    let description = prompter.prompt_optional("Description (optional)")?;
    Ok(CreateEnvironmentRequest {
        project: project.to_string(),
        name,
        display_name: Some(display),
        description,
    })
}

fn entry_to_resolved(entry: &EnvironmentListEntry) -> ResolvedEnvironment {
    ResolvedEnvironment {
        id: entry.id,
        name: entry.name.clone(),
        project: entry.project.clone(),
        slug: entry.slug.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use std::cell::RefCell;
    use unisrv_api::models::EnvironmentListResponse;
    use unisrv_api::test_support::MockApiClient;
    use uuid::Uuid;

    /// Scripted prompter: returns answers in order they're requested.
    struct ScriptedPrompter {
        strings: RefCell<Vec<String>>,
        optionals: RefCell<Vec<Option<String>>>,
    }

    impl ScriptedPrompter {
        fn new(strings: Vec<&str>, optionals: Vec<Option<&str>>) -> Self {
            Self {
                strings: RefCell::new(strings.into_iter().rev().map(String::from).collect()),
                optionals: RefCell::new(
                    optionals
                        .into_iter()
                        .rev()
                        .map(|o| o.map(String::from))
                        .collect(),
                ),
            }
        }
    }

    impl Prompter for ScriptedPrompter {
        fn prompt_string(&self, _prompt: &str, default: Option<&str>) -> Result<String> {
            let answer = self.strings.borrow_mut().pop().expect("no scripted string");
            if answer.is_empty() {
                Ok(default.unwrap_or("").to_string())
            } else {
                Ok(answer)
            }
        }
        fn prompt_optional(&self, _prompt: &str) -> Result<Option<String>> {
            Ok(self
                .optionals
                .borrow_mut()
                .pop()
                .expect("no scripted optional"))
        }
    }

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

    #[tokio::test]
    async fn single_match_returns_use() {
        let client =
            MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
                environments: vec![entry("prod", "demo")],
            }));
        let prompter = ScriptedPrompter::new(vec![], vec![]);
        let action = resolve(&client, "demo", None, &prompter).await.unwrap();
        assert!(matches!(action, EnvAction::Use(e) if e.name == "prod"));
    }

    #[tokio::test]
    async fn no_match_prompts_for_create_with_defaults() {
        let client =
            MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
                environments: vec![],
            }));
        // Empty answers → defaults kick in.
        let prompter = ScriptedPrompter::new(vec!["", ""], vec![None]);
        let action = resolve(&client, "nginx-demo", None, &prompter)
            .await
            .unwrap();
        match action {
            EnvAction::Create(req) => {
                assert_eq!(req.name, "prod");
                assert_eq!(req.display_name.as_deref(), Some("nginx-demo Production"));
                assert_eq!(req.description, None);
                assert_eq!(req.project, "nginx-demo");
            }
            _ => panic!("expected Create"),
        }
    }

    #[tokio::test]
    async fn no_match_prompts_for_create_with_user_answers() {
        let client =
            MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
                environments: vec![],
            }));
        let prompter = ScriptedPrompter::new(vec!["staging", "Staging Env"], vec![Some("for QA")]);
        let action = resolve(&client, "demo", None, &prompter).await.unwrap();
        match action {
            EnvAction::Create(req) => {
                assert_eq!(req.name, "staging");
                assert_eq!(req.display_name.as_deref(), Some("Staging Env"));
                assert_eq!(req.description.as_deref(), Some("for QA"));
            }
            _ => panic!("expected Create"),
        }
    }

    #[tokio::test]
    async fn ambiguous_without_flag_errors() {
        let client =
            MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
                environments: vec![entry("prod", "demo"), entry("staging", "demo")],
            }));
        let prompter = ScriptedPrompter::new(vec![], vec![]);
        let err = resolve(&client, "demo", None, &prompter).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("--env"), "msg: {msg}");
        assert!(msg.contains("prod"), "msg: {msg}");
        assert!(msg.contains("staging"), "msg: {msg}");
    }

    #[tokio::test]
    async fn env_flag_picks_named_env() {
        let client =
            MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
                environments: vec![entry("prod", "demo"), entry("staging", "demo")],
            }));
        let prompter = ScriptedPrompter::new(vec![], vec![]);
        let action = resolve(&client, "demo", Some("staging"), &prompter)
            .await
            .unwrap();
        assert!(matches!(action, EnvAction::Use(e) if e.name == "staging"));
    }

    #[tokio::test]
    async fn env_flag_with_unknown_name_prompts_create() {
        let client =
            MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
                environments: vec![entry("prod", "demo")],
            }));
        let prompter = ScriptedPrompter::new(vec!["", ""], vec![None]);
        let action = resolve(&client, "demo", Some("staging"), &prompter)
            .await
            .unwrap();
        match action {
            EnvAction::Create(req) => assert_eq!(req.name, "staging"),
            _ => panic!("expected Create"),
        }
    }

    #[tokio::test]
    async fn ignores_envs_in_other_projects() {
        let client =
            MockApiClient::logged_in().with_list_environments(Ok(EnvironmentListResponse {
                environments: vec![entry("prod", "other"), entry("prod", "demo")],
            }));
        let prompter = ScriptedPrompter::new(vec![], vec![]);
        let action = resolve(&client, "demo", None, &prompter).await.unwrap();
        assert!(matches!(action, EnvAction::Use(e) if e.project == "demo"));
    }
}
