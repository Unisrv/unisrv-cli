//! Pre-apply validation: every host referenced in HCL must already be claimed,
//! with a valid certificate. Surfaces actionable error messages.

use anyhow::{Result, bail};
use chrono::Utc;
use std::collections::BTreeSet;
use unisrv_api::ApiClient;
use unisrv_api::models::HostResponse;

use super::desired::DesiredState;

#[allow(dead_code)]
pub async fn validate_hosts(client: &dyn ApiClient, desired: &DesiredState) -> Result<()> {
    let referenced: BTreeSet<&str> = desired.services.values().map(|s| s.host.as_str()).collect();
    if referenced.is_empty() {
        return Ok(());
    }
    let claimed = client.list_hosts().await?;
    validate_hosts_against(&referenced, &claimed)
}

pub fn validate_hosts_against(referenced: &BTreeSet<&str>, claimed: &[HostResponse]) -> Result<()> {
    if referenced.is_empty() {
        return Ok(());
    }
    let by_host: std::collections::BTreeMap<&str, &HostResponse> =
        claimed.iter().map(|h| (h.host.as_str(), h)).collect();
    let now = Utc::now().naive_utc();
    let mut problems: Vec<String> = Vec::new();
    for host in referenced {
        match by_host.get(host) {
            None => problems.push(format!(
                "host {host:?} is not claimed. Run: unisrv host claim {host}"
            )),
            Some(h) => {
                let cert_ok = h.certificate_type.is_some()
                    && h.certificate_valid_until
                        .map(|until| until > now)
                        .unwrap_or(false);
                if !cert_ok {
                    problems.push(format!(
                        "host {host:?} has no valid TLS certificate yet. Wait for provisioning, or run: unisrv host claim {host}"
                    ));
                }
            }
        }
    }
    if !problems.is_empty() {
        bail!("preflight failed:\n  - {}", problems.join("\n  - "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDateTime};
    use std::collections::BTreeMap;
    use unisrv_api::models::HTTPServiceConfig;
    use unisrv_api::test_support::MockApiClient;
    use uuid::Uuid;

    use crate::commands::up::desired::{DesiredService, DesiredState};

    fn desired_with_hosts(hosts: &[&str]) -> DesiredState {
        let mut s = DesiredState {
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        for h in hosts {
            s.services.insert(
                h.to_string(),
                DesiredService {
                    name: h.to_string(),
                    host: h.to_string(),
                    region: "dev".into(),
                    configuration: HTTPServiceConfig {
                        allow_http: false,
                        locations: vec![],
                    },
                },
            );
        }
        s
    }

    fn host_with_cert(host: &str, valid: bool) -> HostResponse {
        let valid_until = if valid {
            Some(Utc::now().naive_utc() + Duration::days(30))
        } else {
            None
        };
        HostResponse {
            id: Uuid::new_v4(),
            host: host.to_string(),
            user_id: Uuid::new_v4(),
            service_id: None,
            certificate_type: if valid {
                Some("letsencrypt".into())
            } else {
                None
            },
            certificate_valid_until: valid_until,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
        }
    }

    #[tokio::test]
    async fn passes_when_all_hosts_claimed_with_certs() {
        let client =
            MockApiClient::logged_in().with_list_hosts(Ok(vec![host_with_cert("a.example", true)]));
        let desired = desired_with_hosts(&["a.example"]);
        validate_hosts(&client, &desired).await.unwrap();
    }

    #[tokio::test]
    async fn fails_when_host_not_claimed() {
        let client = MockApiClient::logged_in().with_list_hosts(Ok(vec![]));
        let desired = desired_with_hosts(&["missing.example"]);
        let err = validate_hosts(&client, &desired).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("missing.example"), "msg: {msg}");
        assert!(msg.contains("unisrv host claim"), "msg: {msg}");
    }

    #[tokio::test]
    async fn fails_when_cert_missing() {
        let client = MockApiClient::logged_in()
            .with_list_hosts(Ok(vec![host_with_cert("h.example", false)]));
        let desired = desired_with_hosts(&["h.example"]);
        let err = validate_hosts(&client, &desired).await.unwrap_err();
        assert!(format!("{err:#}").contains("certificate"));
    }

    #[tokio::test]
    async fn fails_when_cert_expired() {
        let mut h = host_with_cert("h.example", true);
        h.certificate_valid_until = Some(Utc::now().naive_utc() - Duration::days(1));
        let client = MockApiClient::logged_in().with_list_hosts(Ok(vec![h]));
        let desired = desired_with_hosts(&["h.example"]);
        let err = validate_hosts(&client, &desired).await.unwrap_err();
        assert!(format!("{err:#}").contains("certificate"));
    }

    #[tokio::test]
    async fn skips_host_check_when_no_services() {
        // No list_hosts call configured — would panic if invoked.
        let client = MockApiClient::logged_in();
        let desired = DesiredState {
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };
        validate_hosts(&client, &desired).await.unwrap();
    }
}
