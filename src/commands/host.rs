use anyhow::Result;
use chrono::{Duration, NaiveDateTime};
use chrono_humanize::HumanTime;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table, presets::UTF8_FULL};
use dialoguer::Confirm;
use unisrv_api::ApiClient;
use unisrv_api::models::{CertificateType, ClaimHostRequest, DnsConfigResponse, HostResponse};

pub async fn claim(client: &dyn ApiClient, hostname: &str) -> Result<()> {
    claim_with_confirm(client, hostname, prompt_dns_confirmation)
        .await
        .map(|_| ())
}

/// Claim and provision a `*.unisrv.dev` host non-interactively. DNS for these
/// domains is preconfigured, so the claim flow never reaches the DNS prompt.
/// Used by `unisrv up` to auto-claim managed subdomains during preflight.
pub(crate) async fn provision_managed_host(
    client: &dyn ApiClient,
    hostname: &str,
) -> Result<HostResponse> {
    debug_assert!(
        is_unisrv_managed_domain(hostname),
        "provision_managed_host is only valid for *.unisrv.dev hosts"
    );
    claim_with_confirm(client, hostname, || {
        Err(anyhow::anyhow!(
            "claim for managed host unexpectedly required DNS confirmation; \
             the API returned an unrecognized hostname"
        ))
    })
    .await
}

fn prompt_dns_confirmation() -> Result<bool> {
    Ok(Confirm::new()
        .with_prompt("DNS records configured?")
        .default(false)
        .interact()?)
}

async fn claim_with_confirm<F>(
    client: &dyn ApiClient,
    hostname: &str,
    confirm: F,
) -> Result<HostResponse>
where
    F: FnOnce() -> Result<bool>,
{
    let host = client
        .claim_host(ClaimHostRequest {
            // Canonicalize: DNS is case-insensitive and the server stores hosts
            // verbatim, so claim the same spelling `up` will link/compare against.
            host: normalize_host(hostname),
        })
        .await?;

    // Base-domain (`*.unisrv.dev`) hosts are served by the platform wildcard
    // certificate; the claim stamps `common_wildcard` and that is sufficient (a
    // per-host cert request would 400). Verify the stamp actually landed before
    // reporting success — otherwise we'd claim success on a cert-less host that
    // preflight would then reject.
    if is_unisrv_managed_domain(&host.host) {
        if host.certificate_type == Some(CertificateType::CommonWildcard) {
            println!(
                "\u{2713} Claimed {}. Served by the platform wildcard certificate.",
                host.host
            );
            return Ok(host);
        }
        return Err(anyhow::anyhow!(
            "claimed {} but the platform did not stamp a wildcard certificate (got {:?}); \
             a base-domain host cannot use a per-host certificate",
            host.host,
            host.certificate_type
        ));
    }

    if cert_in_lockout(&host, chrono::Utc::now().naive_utc()) {
        let valid_until = host
            .certificate_valid_until
            .expect("lockout requires a valid_until");
        println!(
            "\u{2713} {} is already provisioned. Certificate valid until {}.",
            host.host, valid_until
        );
        return Ok(host);
    }

    // Managed (`*.unisrv.dev`) hosts already returned above, so everything from
    // here is an external host that needs DNS set up before a per-host cert.
    let cert_exists = host.certificate_valid_until.is_some();

    if !cert_exists {
        let dns = client.get_hosts_dns_config().await?;
        print_dns_records(&host.host, &dns);

        if !confirm()? {
            println!(
                "Aborted. Re-run `unisrv host claim {}` once DNS is configured.",
                host.host
            );
            return Ok(host);
        }
    }

    let host = client.request_host_cert(host.id).await?;
    let valid_until = host
        .certificate_valid_until
        .ok_or_else(|| anyhow::anyhow!("Certificate request returned without expiry"))?;
    println!(
        "\u{1f512} Certificate provisioned for {}. Valid until {}.",
        host.host, valid_until
    );
    Ok(host)
}

/// Canonical form for comparing hostnames: lowercased, trailing dot stripped.
/// DNS names are case-insensitive and an FQDN may carry a trailing root dot, so
/// two spellings of the same host must compare equal.
pub(crate) fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

pub(crate) fn is_unisrv_managed_domain(host: &str) -> bool {
    normalize_host(host).ends_with(".unisrv.dev")
}

fn cert_in_lockout(host: &HostResponse, now: chrono::NaiveDateTime) -> bool {
    // Without a certificate type there is no real cert, regardless of any
    // valid_until the API may report — so it cannot be in a renewal lockout.
    if host.certificate_type.is_none() {
        return false;
    }
    let Some(valid_until) = host.certificate_valid_until else {
        return false;
    };
    let issued_at = host.updated_at;
    let lifetime = valid_until - issued_at;
    let earliest_renewal = issued_at + lifetime / 2;
    now < earliest_renewal
}

fn print_dns_records(host: &str, dns: &DnsConfigResponse) {
    println!();
    println!("Configure these DNS records for {host}:");
    println!();
    for ip in &dns.ipv4_addresses {
        println!("  A     {host}    {ip}");
    }
    for ip in &dns.ipv6_addresses {
        println!("  AAAA  {host}    {ip}");
    }
    println!();
}

pub async fn list(client: &dyn ApiClient, json: bool) -> Result<()> {
    let hosts = client.list_hosts().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&hosts)?);
        return Ok(());
    }

    if hosts.is_empty() {
        println!("No hosts claimed yet. Run `unisrv host claim <hostname>` to add one.");
        return Ok(());
    }

    let use_color = console::Term::stdout().features().colors_supported();
    let now = chrono::Utc::now().naive_utc();
    println!("{}", render_table(&hosts, now, use_color));
    Ok(())
}

fn render_table(hosts: &[HostResponse], now: NaiveDateTime, use_color: bool) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        Cell::new("HOST").add_attribute(Attribute::Bold),
        Cell::new("CERT").add_attribute(Attribute::Bold),
        Cell::new("EXPIRES").add_attribute(Attribute::Bold),
        Cell::new("ATTACHED").add_attribute(Attribute::Bold),
        Cell::new("CREATED").add_attribute(Attribute::Bold),
    ]);

    for host in hosts {
        let (cert_text, cert_color) = format_cert_type(host.certificate_type);
        let (expires_text, expires_color) = format_expires(host.certificate_valid_until, now);
        let (attached_text, attached_color) = format_attached(host.service_id.is_some());
        let created = format_relative(host.created_at, now);

        table.add_row(vec![
            Cell::new(&host.host),
            cell_with_color(cert_text, cert_color, use_color),
            cell_with_color(expires_text, expires_color, use_color),
            cell_with_color(attached_text, attached_color, use_color),
            Cell::new(created),
        ]);
    }
    table.to_string()
}

fn cell_with_color(text: String, color: Option<Color>, use_color: bool) -> Cell {
    let cell = Cell::new(text);
    match (color, use_color) {
        (Some(c), true) => cell.fg(c),
        _ => cell,
    }
}

fn format_cert_type(cert_type: Option<CertificateType>) -> (String, Option<Color>) {
    match cert_type {
        None => ("\u{2014}".into(), Some(Color::DarkGrey)),
        Some(CertificateType::CommonWildcard) => ("wildcard".into(), None),
        Some(CertificateType::LetsEncrypt) => ("LE".into(), None),
        Some(CertificateType::Custom) => ("custom".into(), None),
        Some(CertificateType::Unknown) => ("?".into(), Some(Color::DarkGrey)),
    }
}

fn format_expires(
    valid_until: Option<NaiveDateTime>,
    now: NaiveDateTime,
) -> (String, Option<Color>) {
    let Some(valid_until) = valid_until else {
        return ("\u{2014}".into(), Some(Color::DarkGrey));
    };
    let delta = valid_until - now;
    let text = HumanTime::from(delta).to_string();
    if delta < Duration::zero() {
        (text, Some(Color::Red))
    } else if delta < Duration::days(30) {
        (text, Some(Color::Yellow))
    } else {
        (text, Some(Color::Green))
    }
}

fn format_attached(attached: bool) -> (String, Option<Color>) {
    if attached {
        ("yes".into(), None)
    } else {
        ("no".into(), Some(Color::DarkGrey))
    }
}

fn format_relative(when: NaiveDateTime, now: NaiveDateTime) -> String {
    HumanTime::from(when - now).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use std::net::{Ipv4Addr, Ipv6Addr};
    use unisrv_api::ApiError;
    use unisrv_api::test_support::MockApiClient;
    use uuid::Uuid;

    fn host_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-00000000beef").unwrap()
    }

    fn user_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-0000000000ff").unwrap()
    }

    fn unprovisioned_host() -> HostResponse {
        let now = Utc::now().naive_utc();
        HostResponse {
            id: host_id(),
            host: "example.com".into(),
            user_id: user_id(),
            service_id: None,
            certificate_type: None,
            certificate_valid_until: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn provisioned_host(issued_days_ago: i64, lifetime_days: i64) -> HostResponse {
        let now = Utc::now().naive_utc();
        let issued_at = now - Duration::days(issued_days_ago);
        let valid_until = issued_at + Duration::days(lifetime_days);
        HostResponse {
            id: host_id(),
            host: "example.com".into(),
            user_id: user_id(),
            service_id: None,
            certificate_type: Some(CertificateType::LetsEncrypt),
            certificate_valid_until: Some(valid_until),
            created_at: issued_at,
            updated_at: issued_at,
        }
    }

    fn dns_config() -> DnsConfigResponse {
        DnsConfigResponse {
            ipv4_addresses: vec![Ipv4Addr::new(198, 51, 100, 10)],
            ipv6_addresses: vec![Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x10)],
        }
    }

    #[tokio::test]
    async fn full_flow_claims_dns_and_provisions_cert() {
        let mock = MockApiClient::logged_in()
            .with_claim_host(Ok(unprovisioned_host()))
            .with_dns_config(Ok(dns_config()))
            .with_request_host_cert(Ok(provisioned_host(0, 90)));

        let result = claim_with_confirm(&mock, "example.com", || Ok(true)).await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert_eq!(calls.claim_host_calls[0].host, "example.com");
        assert_eq!(calls.get_hosts_dns_config_calls, 1);
        assert_eq!(calls.request_host_cert_calls, vec![host_id()]);
    }

    #[tokio::test]
    async fn claim_normalizes_hostname_before_sending() {
        // DNS is case-insensitive and FQDNs may carry a trailing dot; the server
        // stores hosts verbatim. Canonicalize so a claim matches what `up` links
        // (and so an uppercase *.unisrv.dev label doesn't 400 at claim).
        let mock = MockApiClient::logged_in().with_claim_host(Ok(provisioned_host(1, 90)));
        let _ = claim_with_confirm(&mock, "Example.COM.", || Ok(true)).await;
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls[0].host, "example.com");
    }

    #[tokio::test]
    async fn already_provisioned_host_skips_dns_and_cert() {
        let mock = MockApiClient::logged_in().with_claim_host(Ok(provisioned_host(1, 90)));

        let result = claim_with_confirm(&mock, "example.com", || {
            panic!("confirmation prompt should not be invoked for an already-provisioned host")
        })
        .await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert_eq!(calls.get_hosts_dns_config_calls, 0);
        assert!(calls.request_host_cert_calls.is_empty());
    }

    #[tokio::test]
    async fn cert_past_lockout_window_is_renewed_without_dns_prompt() {
        let mock = MockApiClient::logged_in()
            .with_claim_host(Ok(provisioned_host(60, 90)))
            .with_request_host_cert(Ok(provisioned_host(0, 90)));

        let result = claim_with_confirm(&mock, "example.com", || {
            panic!("DNS prompt should be skipped when a cert already exists")
        })
        .await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.get_hosts_dns_config_calls, 0);
        assert_eq!(calls.request_host_cert_calls, vec![host_id()]);
    }

    #[tokio::test]
    async fn unisrv_dev_domain_claims_without_cert_request() {
        // Base-domain hosts are served by the platform wildcard certificate:
        // claiming stamps `common_wildcard` and is sufficient. Requesting a
        // per-host cert would 400, so the claim flow must skip it (and the DNS
        // prompt) entirely.
        let mut claimed = unprovisioned_host();
        claimed.host = "demo.unisrv.dev".into();
        claimed.certificate_type = Some(CertificateType::CommonWildcard);

        let mock = MockApiClient::logged_in().with_claim_host(Ok(claimed));

        let result = claim_with_confirm(&mock, "demo.unisrv.dev", || {
            panic!("DNS prompt should be skipped for unisrv.dev subdomains")
        })
        .await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert_eq!(calls.get_hosts_dns_config_calls, 0);
        assert!(
            calls.request_host_cert_calls.is_empty(),
            "base-domain host must not request a per-host cert"
        );
    }

    #[tokio::test]
    async fn managed_host_without_wildcard_cert_errors_instead_of_false_success() {
        // The early-return success path must verify the backend actually stamped
        // the wildcard cert. If it didn't (cert_type None/other), don't claim
        // success — error clearly, and never request a doomed per-host cert.
        let mut claimed = unprovisioned_host(); // certificate_type: None
        claimed.host = "demo.unisrv.dev".into();
        let mock = MockApiClient::logged_in().with_claim_host(Ok(claimed));

        let err = claim_with_confirm(&mock, "demo.unisrv.dev", || {
            panic!("DNS prompt should be skipped for unisrv.dev subdomains")
        })
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("wildcard"),
            "expected a wildcard-cert error, got: {err}"
        );
        assert!(
            mock.calls
                .lock()
                .unwrap()
                .request_host_cert_calls
                .is_empty(),
            "must not request a per-host cert for a base-domain host"
        );
    }

    #[test]
    fn is_unisrv_managed_domain_matches_subdomains_only() {
        assert!(is_unisrv_managed_domain("foo.unisrv.dev"));
        assert!(is_unisrv_managed_domain("a.b.unisrv.dev"));
        assert!(is_unisrv_managed_domain("Foo.UNISRV.DEV"));
        assert!(is_unisrv_managed_domain("foo.unisrv.dev."));

        assert!(!is_unisrv_managed_domain("unisrv.dev"));
        assert!(!is_unisrv_managed_domain("evilunisrv.dev"));
        assert!(!is_unisrv_managed_domain("foo.unisrv.dev.evil.com"));
        assert!(!is_unisrv_managed_domain("example.com"));
    }

    #[tokio::test]
    async fn user_declining_dns_prompt_skips_cert_request() {
        let mock = MockApiClient::logged_in()
            .with_claim_host(Ok(unprovisioned_host()))
            .with_dns_config(Ok(dns_config()));

        let result = claim_with_confirm(&mock, "example.com", || Ok(false)).await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.claim_host_calls.len(), 1);
        assert_eq!(calls.get_hosts_dns_config_calls, 1);
        assert!(calls.request_host_cert_calls.is_empty());
    }

    #[tokio::test]
    async fn claim_host_error_propagates() {
        let mock = MockApiClient::logged_in().with_claim_host(Err(ApiError::Server {
            status: 409,
            reason: "Hostname is already in use".into(),
        }));

        let result = claim_with_confirm(&mock, "example.com", || {
            panic!("confirm should not run when claim fails")
        })
        .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("409"));
        assert!(err.to_string().contains("already in use"));

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.get_hosts_dns_config_calls, 0);
        assert!(calls.request_host_cert_calls.is_empty());
    }

    #[tokio::test]
    async fn cert_request_error_propagates() {
        let mock = MockApiClient::logged_in()
            .with_claim_host(Ok(unprovisioned_host()))
            .with_dns_config(Ok(dns_config()))
            .with_request_host_cert(Err(ApiError::Server {
                status: 400,
                reason: "DNS validation failed: A record does not point at allowed IP".into(),
            }));

        let result = claim_with_confirm(&mock, "example.com", || Ok(true)).await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("DNS validation failed"));

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.request_host_cert_calls, vec![host_id()]);
    }

    #[tokio::test]
    async fn provision_managed_host_errors_when_claim_returns_unmanaged_host() {
        // Defensive: if the API ever returns a host whose name is not a managed
        // *.unisrv.dev domain and carries no cert, the DNS-confirmation branch
        // is reached. It must surface a clean error, not panic the CLI.
        let mut unexpected = unprovisioned_host();
        unexpected.host = "elsewhere.example.com".into();
        let mock = MockApiClient::logged_in()
            .with_claim_host(Ok(unexpected))
            .with_dns_config(Ok(dns_config()));

        let err = provision_managed_host(&mock, "good.unisrv.dev")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unexpected"),
            "expected an unexpected-DNS-prompt error, got: {err}"
        );
    }

    #[tokio::test]
    async fn requests_cert_when_valid_until_set_but_no_cert_type() {
        // Odd server state: a future valid_until with no certificate_type is not
        // a real cert. It must not be mistaken for "already provisioned" (which
        // would return a no-op success and leave the host without a cert) — we
        // must still request the certificate.
        let mut claimed = unprovisioned_host();
        claimed.certificate_valid_until = Some(Utc::now().naive_utc() + Duration::days(30));
        // certificate_type stays None: no actual cert exists.
        let mock = MockApiClient::logged_in()
            .with_claim_host(Ok(claimed))
            .with_request_host_cert(Ok(provisioned_host(0, 90)));

        let result = claim_with_confirm(&mock, "example.com", || {
            panic!("DNS prompt should be skipped when a valid_until is already present")
        })
        .await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.request_host_cert_calls, vec![host_id()]);
    }

    #[test]
    fn cert_in_lockout_handles_missing_expiry() {
        let host = unprovisioned_host();
        assert!(!cert_in_lockout(&host, Utc::now().naive_utc()));
    }

    #[test]
    fn cert_in_lockout_is_false_without_a_cert_type() {
        // valid_until in the lockout window but no certificate_type → there is
        // no real cert to be locked out of renewing.
        let mut host = provisioned_host(10, 90);
        host.certificate_type = None;
        assert!(!cert_in_lockout(&host, Utc::now().naive_utc()));
    }

    #[test]
    fn cert_in_lockout_locks_out_for_first_half_of_lifetime() {
        let host = provisioned_host(10, 90); // 10 days into 90-day cert; lockout until day 45
        assert!(cert_in_lockout(&host, Utc::now().naive_utc()));
    }

    #[test]
    fn cert_in_lockout_releases_after_half_of_lifetime() {
        let host = provisioned_host(50, 90); // 50 days into 90-day cert; lockout was until day 45
        assert!(!cert_in_lockout(&host, Utc::now().naive_utc()));
    }

    // ── list ──

    fn host_with(
        name: &str,
        cert_type: Option<CertificateType>,
        valid_until: Option<NaiveDateTime>,
        attached: bool,
        created_at: NaiveDateTime,
    ) -> HostResponse {
        HostResponse {
            id: Uuid::new_v4(),
            host: name.into(),
            user_id: user_id(),
            service_id: attached.then(Uuid::new_v4),
            certificate_type: cert_type,
            certificate_valid_until: valid_until,
            created_at,
            updated_at: created_at,
        }
    }

    #[tokio::test]
    async fn list_calls_api_once() {
        let mock = MockApiClient::logged_in().with_list_hosts(Ok(vec![]));
        let result = list(&mock, false).await;
        assert!(result.is_ok(), "expected ok, got {result:?}");
        assert_eq!(mock.calls.lock().unwrap().list_hosts_calls, 1);
    }

    #[tokio::test]
    async fn list_json_with_empty_array() {
        let mock = MockApiClient::logged_in().with_list_hosts(Ok(vec![]));
        let result = list(&mock, true).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn list_propagates_api_error() {
        let mock = MockApiClient::logged_in().with_list_hosts(Err(ApiError::Server {
            status: 500,
            reason: "internal".into(),
        }));
        let result = list(&mock, false).await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("500"));
    }

    #[test]
    fn render_table_includes_host_and_status_columns() {
        let now = Utc::now().naive_utc();
        let hosts = vec![
            host_with(
                "healthy.example.com",
                Some(CertificateType::LetsEncrypt),
                Some(now + Duration::days(73)),
                true,
                now - Duration::days(3),
            ),
            host_with(
                "expiring.example.com",
                Some(CertificateType::LetsEncrypt),
                Some(now + Duration::days(12)),
                false,
                now - Duration::hours(1),
            ),
            host_with(
                "expired.example.com",
                Some(CertificateType::LetsEncrypt),
                Some(now - Duration::days(4)),
                false,
                now - Duration::days(92),
            ),
            host_with("fresh.example.com", None, None, false, now),
        ];

        let rendered = render_table(&hosts, now, false);

        assert!(rendered.contains("HOST"));
        assert!(rendered.contains("CERT"));
        assert!(rendered.contains("EXPIRES"));
        assert!(rendered.contains("ATTACHED"));
        assert!(rendered.contains("CREATED"));
        for name in [
            "healthy.example.com",
            "expiring.example.com",
            "expired.example.com",
            "fresh.example.com",
        ] {
            assert!(
                rendered.contains(name),
                "missing host {name} in:\n{rendered}"
            );
        }
        assert!(rendered.contains("LE"));
        assert!(rendered.contains("\u{2014}")); // em dash for missing values
    }

    #[test]
    fn format_expires_buckets() {
        let now = Utc::now().naive_utc();

        let (text, color) = format_expires(None, now);
        assert_eq!(text, "\u{2014}");
        assert_eq!(color, Some(Color::DarkGrey));

        let (_, color) = format_expires(Some(now + Duration::days(60)), now);
        assert_eq!(color, Some(Color::Green));

        let (_, color) = format_expires(Some(now + Duration::days(10)), now);
        assert_eq!(color, Some(Color::Yellow));

        let (_, color) = format_expires(Some(now - Duration::days(1)), now);
        assert_eq!(color, Some(Color::Red));
    }

    #[test]
    fn format_cert_type_shortens_letsencrypt_and_dims_missing() {
        let (text, color) = format_cert_type(None);
        assert_eq!(text, "\u{2014}");
        assert_eq!(color, Some(Color::DarkGrey));

        let (text, color) = format_cert_type(Some(CertificateType::LetsEncrypt));
        assert_eq!(text, "LE");
        assert_eq!(color, None);

        let (text, _) = format_cert_type(Some(CertificateType::CommonWildcard));
        assert_eq!(text, "wildcard");

        let (text, _) = format_cert_type(Some(CertificateType::Custom));
        assert_eq!(text, "custom");
    }

    #[test]
    fn format_attached_dims_when_unattached() {
        let (text, color) = format_attached(true);
        assert_eq!(text, "yes");
        assert_eq!(color, None);

        let (text, color) = format_attached(false);
        assert_eq!(text, "no");
        assert_eq!(color, Some(Color::DarkGrey));
    }
}
