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
    let referenced: BTreeSet<&str> = desired
        .services
        .values()
        .flat_map(|s| s.hosts.iter().map(String::as_str))
        .collect();
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

/// A host is ready to serve when it has a usable certificate:
///  * `common_wildcard` — served by the platform `*.unisrv.dev` wildcard cert,
///    which has no per-host expiry. Ready as soon as it's claimed.
///  * `lets_encrypt` / `custom` — ready only while their per-host cert is valid.
///  * no cert type — not ready.
fn has_valid_cert(host: &HostResponse, now: chrono::NaiveDateTime) -> bool {
    use unisrv_api::models::CertificateType;
    match host.certificate_type {
        Some(CertificateType::CommonWildcard) => true,
        // Unknown is treated like a per-host cert: ready only while it has an
        // unexpired validity. Conservative for a future backend variant — never
        // reports a cert-less host as ready.
        Some(CertificateType::LetsEncrypt | CertificateType::Custom | CertificateType::Unknown) => {
            host.certificate_valid_until
                .map(|until| until > now)
                .unwrap_or(false)
        }
        None => false,
    }
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

/// Reject any referenced host that is currently bound to a service this
/// environment does not manage. Hosts are global, so a host owned by a service
/// in another environment (or attached out-of-band) must NOT be silently
/// re-linked here — apply would otherwise 409 at link time, mid-mutation. This
/// runs after `fetch_current_state`, so `managed_service_ids` is the set of
/// service ids currently live in this environment (a host bound to one of them
/// is fine: it is kept, updated, or freed by a delete cascade).
pub fn validate_host_ownership(
    desired: &DesiredState,
    hosts: &[HostResponse],
    managed_service_ids: &std::collections::BTreeSet<uuid::Uuid>,
) -> Result<()> {
    let referenced: BTreeSet<String> = desired
        .services
        .values()
        .flat_map(|s| s.hosts.iter().map(|h| normalize_host(h)))
        .collect();
    let by_host: std::collections::BTreeMap<String, &HostResponse> =
        hosts.iter().map(|h| (normalize_host(&h.host), h)).collect();
    for host in &referenced {
        let Some(h) = by_host.get(host) else { continue };
        if let Some(sid) = h.service_id
            && !managed_service_ids.contains(&sid)
        {
            bail!(
                "host {host:?} is already bound to another service outside this environment. \
                 Unlink it there before binding it here."
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDateTime};
    use std::collections::BTreeMap;
    use unisrv_api::ApiError;
    use unisrv_api::models::{CertificateType, HTTPServiceConfig};
    use unisrv_api::test_support::MockApiClient;
    use uuid::Uuid;

    /// A claimed base-domain host: stamped `common_wildcard` at claim, no expiry.
    fn wildcard_host(host: &str) -> HostResponse {
        let mut h = host_with_cert(host, false);
        h.certificate_type = Some(CertificateType::CommonWildcard);
        h
    }

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
                    hosts: vec![h.to_string()],
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
                Some(unisrv_api::models::CertificateType::LetsEncrypt)
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
    async fn ready_claims_unclaimed_unisrv_dev_host_without_cert_request() {
        // An unclaimed *.unisrv.dev host is auto-claimed; the claim stamps
        // `common_wildcard` and is immediately ready. No per-host cert request
        // (that would 400), no DNS prompt.
        let client = MockApiClient::logged_in()
            .with_list_hosts(Ok(vec![]))
            .with_claim_host(Ok(wildcard_host("test.unisrv.dev")));
        let desired = desired_with_hosts(&["test.unisrv.dev"]);

        let hosts = ensure_hosts_ready(&client, &desired).await.unwrap();

        // Returned list reflects the freshly claimed, wildcard-covered host.
        let host = hosts.iter().find(|h| h.host == "test.unisrv.dev").unwrap();
        assert_eq!(host.certificate_type, Some(CertificateType::CommonWildcard));

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert_eq!(calls.claim_host_calls[0].host, "test.unisrv.dev");
        assert!(calls.request_host_cert_calls.is_empty());
        assert_eq!(calls.get_hosts_dns_config_calls, 0);
    }

    #[tokio::test]
    async fn ready_reclaims_certless_unisrv_dev_host_without_cert_request() {
        // Host row exists but isn't yet wildcard-ready (e.g. an interrupted
        // claim). Re-claiming stamps `common_wildcard`; still no cert request.
        let client = MockApiClient::logged_in()
            .with_list_hosts(Ok(vec![host_with_cert("test.unisrv.dev", false)]))
            .with_claim_host(Ok(wildcard_host("test.unisrv.dev")));
        let desired = desired_with_hosts(&["test.unisrv.dev"]);

        let hosts = ensure_hosts_ready(&client, &desired).await.unwrap();

        let host = hosts.iter().find(|h| h.host == "test.unisrv.dev").unwrap();
        assert_eq!(host.certificate_type, Some(CertificateType::CommonWildcard));

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert!(calls.request_host_cert_calls.is_empty());
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
            .with_claim_host(Ok(wildcard_host("test.unisrv.dev")));
        let desired = desired_with_hosts(&["app.example.com", "test.unisrv.dev"]);

        let err = ensure_hosts_ready(&client, &desired).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("app.example.com"),
            "should fail on the custom domain"
        );

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert_eq!(calls.claim_host_calls[0].host, "test.unisrv.dev");
        assert!(calls.request_host_cert_calls.is_empty());
    }

    // ── validate_hosts_against: cert edge cases (a claimed custom domain whose
    //    cert is missing/expired is never auto-fixed, so it surfaces here) ──

    #[test]
    fn validate_accepts_common_wildcard_host_without_expiry() {
        // A claimed *.unisrv.dev host is served by the platform wildcard cert:
        // certificate_type = common_wildcard, no per-host valid_until. It must
        // count as ready even though it has no expiry.
        let h = wildcard_host("app.unisrv.dev");
        let referenced: BTreeSet<&str> = ["app.unisrv.dev"].into_iter().collect();
        assert!(validate_hosts_against(&referenced, &[h], Utc::now().naive_utc()).is_ok());
    }

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

    #[test]
    fn unknown_cert_type_is_ready_only_with_unexpired_validity() {
        let now = Utc::now().naive_utc();
        let mut h = host_with_cert("x.example.com", true);
        h.certificate_type = Some(CertificateType::Unknown);
        h.certificate_valid_until = Some(now + Duration::days(10));
        assert!(has_valid_cert(&h, now), "unknown + future expiry → ready");
        h.certificate_valid_until = Some(now - Duration::days(1));
        assert!(!has_valid_cert(&h, now), "unknown + expired → not ready");
        h.certificate_valid_until = None;
        assert!(!has_valid_cert(&h, now), "unknown + no expiry → not ready");
    }

    #[test]
    fn ownership_rejects_host_bound_to_a_service_this_env_does_not_manage() {
        // Hosts are global. A referenced host bound to a service NOT in this
        // environment must error before any mutation — we never steal it.
        let other_service = Uuid::new_v4();
        let mut h = host_with_cert("shop.example.com", true);
        h.service_id = Some(other_service);
        let desired = desired_with_hosts(&["shop.example.com"]);
        let managed: BTreeSet<Uuid> = BTreeSet::new(); // env manages no services

        let err = validate_host_ownership(&desired, &[h], &managed).unwrap_err();
        assert!(
            format!("{err:#}").contains("another service"),
            "got: {err:#}"
        );
    }

    #[test]
    fn ownership_allows_free_host_or_host_on_a_managed_service() {
        let svc = Uuid::new_v4();
        let managed: BTreeSet<Uuid> = [svc].into_iter().collect();
        let desired = desired_with_hosts(&["a.example.com", "b.example.com"]);
        // a.* is bound to a managed service (kept/updated/deleted here); b.* is free.
        let mut bound = host_with_cert("a.example.com", true);
        bound.service_id = Some(svc);
        let free = host_with_cert("b.example.com", true); // service_id: None
        assert!(validate_host_ownership(&desired, &[bound, free], &managed).is_ok());
    }
}
