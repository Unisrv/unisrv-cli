//! Pre-apply host readiness. Every host referenced in HCL must end up claimed
//! with a valid certificate. Managed `*.unisrv.dev` hosts are auto-claimed and
//! provisioned here (a mutating step, not pure validation); custom domains must
//! already be ready and otherwise surface actionable error messages.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::BTreeSet;
use unisrv_api::ApiClient;
use unisrv_api::models::HostResponse;

use super::desired::DesiredState;
use crate::commands::host::{is_unisrv_managed_domain, normalize_host, provision_managed_host};

/// List hosts, auto-claim/provision fixable `*.unisrv.dev` hosts referenced in
/// the manifest, then validate. Returns the up-to-date host list so the caller
/// can feed it to the rest of the pipeline without a second `list_hosts` call.
pub async fn ensure_hosts_ready(
    client: &dyn ApiClient,
    desired: &DesiredState,
) -> Result<Vec<HostResponse>> {
    let referenced: BTreeSet<&str> = desired.services.values().map(|s| s.host.as_str()).collect();
    let mut hosts = client.list_hosts().await?;
    let now = Utc::now().naive_utc();

    for host in &referenced {
        let ready = hosts
            .iter()
            .any(|h| normalize_host(&h.host) == normalize_host(host) && has_valid_cert(h, now));
        if ready || !is_unisrv_managed_domain(host) {
            continue;
        }
        let provisioned = provision_managed_host(client, host)
            .await
            .with_context(|| format!("failed to claim host {host:?}"))?;
        match hosts
            .iter_mut()
            .find(|h| normalize_host(&h.host) == normalize_host(&provisioned.host))
        {
            Some(existing) => *existing = provisioned,
            None => hosts.push(provisioned),
        }
    }

    validate_hosts_against(&referenced, &hosts, now)?;
    Ok(hosts)
}

/// A host is ready to serve when it carries a certificate that is still valid.
fn has_valid_cert(host: &HostResponse, now: chrono::NaiveDateTime) -> bool {
    host.certificate_type.is_some()
        && host
            .certificate_valid_until
            .map(|until| until > now)
            .unwrap_or(false)
}

pub fn validate_hosts_against(
    referenced: &BTreeSet<&str>,
    claimed: &[HostResponse],
    now: chrono::NaiveDateTime,
) -> Result<()> {
    if referenced.is_empty() {
        return Ok(());
    }
    let by_host: std::collections::BTreeMap<String, &HostResponse> = claimed
        .iter()
        .map(|h| (normalize_host(&h.host), h))
        .collect();
    let mut problems: Vec<String> = Vec::new();
    for host in referenced {
        match by_host.get(&normalize_host(host)) {
            None => problems.push(format!(
                "host {host:?} is not claimed. Run: unisrv host claim {host}"
            )),
            Some(h) => {
                if !has_valid_cert(h, now) {
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
    use unisrv_api::ApiError;
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

    // ── ensure_hosts_ready ──

    #[tokio::test]
    async fn ready_returns_hosts_and_claims_nothing_when_all_valid() {
        let client =
            MockApiClient::logged_in().with_list_hosts(Ok(vec![host_with_cert("a.example", true)]));
        let desired = desired_with_hosts(&["a.example"]);

        let hosts = ensure_hosts_ready(&client, &desired).await.unwrap();

        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host, "a.example");
        let calls = client.calls.lock().unwrap();
        assert!(calls.claim_host_calls.is_empty());
        assert!(calls.request_host_cert_calls.is_empty());
    }

    #[tokio::test]
    async fn ready_matches_reference_case_insensitively() {
        // The manifest references a non-canonical spelling (mixed case) while
        // the API stores the canonical lowercase host. They must be treated as
        // the same host, so an already-valid host is not needlessly re-claimed
        // (which would burn a Let's Encrypt request) nor reported as missing.
        let client = MockApiClient::logged_in()
            .with_list_hosts(Ok(vec![host_with_cert("app.unisrv.dev", true)]));
        let desired = desired_with_hosts(&["App.Unisrv.Dev"]);

        let hosts = ensure_hosts_ready(&client, &desired).await.unwrap();
        assert_eq!(hosts.len(), 1);

        let calls = client.calls.lock().unwrap();
        assert!(
            calls.claim_host_calls.is_empty(),
            "an already-valid host must not be re-claimed"
        );
    }

    #[tokio::test]
    async fn ready_claims_and_provisions_unclaimed_unisrv_dev_host() {
        let client = MockApiClient::logged_in()
            .with_list_hosts(Ok(vec![]))
            .with_claim_host(Ok(host_with_cert("test.unisrv.dev", false)))
            .with_request_host_cert(Ok(host_with_cert("test.unisrv.dev", true)));
        let desired = desired_with_hosts(&["test.unisrv.dev"]);

        let hosts = ensure_hosts_ready(&client, &desired).await.unwrap();

        // Returned list reflects the freshly provisioned host.
        let host = hosts.iter().find(|h| h.host == "test.unisrv.dev").unwrap();
        assert!(host.certificate_type.is_some());

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert_eq!(calls.claim_host_calls[0].host, "test.unisrv.dev");
        assert_eq!(calls.request_host_cert_calls.len(), 1);
        // unisrv.dev DNS is preconfigured — no DNS lookup or prompt.
        assert_eq!(calls.get_hosts_dns_config_calls, 0);
    }

    #[tokio::test]
    async fn ready_provisions_cert_for_claimed_but_certless_unisrv_dev_host() {
        // Host already claimed (e.g. an interrupted claim) but has no cert.
        let client = MockApiClient::logged_in()
            .with_list_hosts(Ok(vec![host_with_cert("test.unisrv.dev", false)]))
            .with_claim_host(Ok(host_with_cert("test.unisrv.dev", false)))
            .with_request_host_cert(Ok(host_with_cert("test.unisrv.dev", true)));
        let desired = desired_with_hosts(&["test.unisrv.dev"]);

        let hosts = ensure_hosts_ready(&client, &desired).await.unwrap();

        let host = hosts.iter().find(|h| h.host == "test.unisrv.dev").unwrap();
        assert!(host.certificate_type.is_some());

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.request_host_cert_calls.len(), 1);
        assert_eq!(calls.get_hosts_dns_config_calls, 0);
    }

    #[tokio::test]
    async fn ready_does_not_claim_custom_domains_and_errors() {
        let client = MockApiClient::logged_in().with_list_hosts(Ok(vec![]));
        let desired = desired_with_hosts(&["app.example.com"]);

        let err = ensure_hosts_ready(&client, &desired).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("app.example.com"), "msg: {msg}");
        assert!(msg.contains("unisrv host claim"), "msg: {msg}");

        let calls = client.calls.lock().unwrap();
        assert!(calls.claim_host_calls.is_empty());
        assert!(calls.request_host_cert_calls.is_empty());
    }

    #[tokio::test]
    async fn ready_fails_fast_naming_host_when_claim_errors() {
        // Two pending unisrv.dev hosts; the first claim fails. We must stop
        // before touching the second (don't burn Let's Encrypt quota).
        let client = MockApiClient::logged_in()
            .with_list_hosts(Ok(vec![]))
            .with_claim_host(Err(ApiError::Server {
                status: 409,
                reason: "Hostname is already in use".into(),
            }));
        let desired = desired_with_hosts(&["a.unisrv.dev", "b.unisrv.dev"]);

        let err = ensure_hosts_ready(&client, &desired).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("a.unisrv.dev"),
            "msg should name the host: {msg}"
        );
        assert!(msg.contains("409"), "msg should surface the cause: {msg}");

        let calls = client.calls.lock().unwrap();
        assert_eq!(
            calls.claim_host_calls.len(),
            1,
            "second host must not be attempted"
        );
    }

    #[tokio::test]
    async fn ready_with_no_services_reads_hosts_but_claims_nothing() {
        // An empty manifest still needs the host list (fetch derives existing
        // services' hostnames from it) but must never claim or provision.
        let client = MockApiClient::logged_in().with_list_hosts(Ok(vec![]));
        let desired = DesiredState {
            project: "demo".into(),
            services: BTreeMap::new(),
            deployments: BTreeMap::new(),
        };

        let hosts = ensure_hosts_ready(&client, &desired).await.unwrap();
        assert!(hosts.is_empty());

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.list_hosts_calls, 1);
        assert!(calls.claim_host_calls.is_empty());
        assert!(calls.request_host_cert_calls.is_empty());
    }

    #[tokio::test]
    async fn ready_claims_unisrv_dev_even_when_custom_domain_blocks_run() {
        // A run with one fixable unisrv.dev host and one custom domain that
        // still needs DNS work: claim the managed host (progress persists for
        // the next run) but still fail on the custom domain.
        let client = MockApiClient::logged_in()
            .with_list_hosts(Ok(vec![]))
            .with_claim_host(Ok(host_with_cert("test.unisrv.dev", false)))
            .with_request_host_cert(Ok(host_with_cert("test.unisrv.dev", true)));
        let desired = desired_with_hosts(&["app.example.com", "test.unisrv.dev"]);

        let err = ensure_hosts_ready(&client, &desired).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("app.example.com"),
            "should fail on the custom domain"
        );

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert_eq!(calls.claim_host_calls[0].host, "test.unisrv.dev");
        assert_eq!(calls.request_host_cert_calls.len(), 1);
    }

    // ── validate_hosts_against: cert edge cases (a claimed custom domain whose
    //    cert is missing/expired is never auto-fixed, so it surfaces here) ──

    #[test]
    fn validate_flags_claimed_host_without_cert() {
        let claimed = vec![host_with_cert("h.example", false)];
        let referenced: BTreeSet<&str> = ["h.example"].into_iter().collect();
        let err =
            validate_hosts_against(&referenced, &claimed, Utc::now().naive_utc()).unwrap_err();
        assert!(format!("{err:#}").contains("certificate"));
    }

    #[test]
    fn validate_flags_claimed_host_with_expired_cert() {
        let mut h = host_with_cert("h.example", true);
        h.certificate_valid_until = Some(Utc::now().naive_utc() - Duration::days(1));
        let referenced: BTreeSet<&str> = ["h.example"].into_iter().collect();
        let err = validate_hosts_against(&referenced, &[h], Utc::now().naive_utc()).unwrap_err();
        assert!(format!("{err:#}").contains("certificate"));
    }
}
