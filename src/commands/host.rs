use anyhow::Result;
use chrono::{Duration, NaiveDateTime};
use chrono_humanize::HumanTime;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table, presets::UTF8_FULL};
use dialoguer::Confirm;
use unisrv_api::ApiClient;
use unisrv_api::models::{ClaimHostRequest, DnsConfigResponse, HostResponse};

pub async fn claim(client: &dyn ApiClient, hostname: &str) -> Result<()> {
    claim_with_confirm(client, hostname, prompt_dns_confirmation).await
}

fn prompt_dns_confirmation() -> Result<bool> {
    Ok(Confirm::new()
        .with_prompt("DNS records configured?")
        .default(false)
        .interact()?)
}

async fn claim_with_confirm<F>(client: &dyn ApiClient, hostname: &str, confirm: F) -> Result<()>
where
    F: FnOnce() -> Result<bool>,
{
    let host = client
        .claim_host(ClaimHostRequest {
            host: hostname.to_string(),
        })
        .await?;

    if cert_in_lockout(&host, chrono::Utc::now().naive_utc()) {
        let valid_until = host
            .certificate_valid_until
            .expect("lockout requires a valid_until");
        println!(
            "\u{2713} {} is already provisioned. Certificate valid until {}.",
            host.host, valid_until
        );
        return Ok(());
    }

    let cert_exists = host.certificate_valid_until.is_some();
    let dns_preconfigured = is_unisrv_managed_domain(&host.host);

    if !cert_exists && !dns_preconfigured {
        let dns = client.get_hosts_dns_config().await?;
        print_dns_records(&host.host, &dns);

        if !confirm()? {
            println!(
                "Aborted. Re-run `unisrv host claim {}` once DNS is configured.",
                host.host
            );
            return Ok(());
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
    Ok(())
}

fn is_unisrv_managed_domain(host: &str) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    normalized.ends_with(".unisrv.dev")
}

fn cert_in_lockout(host: &HostResponse, now: chrono::NaiveDateTime) -> bool {
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
        let (cert_text, cert_color) = format_cert_type(host.certificate_type.as_deref());
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

fn format_cert_type(cert_type: Option<&str>) -> (String, Option<Color>) {
    match cert_type {
        None => ("\u{2014}".into(), Some(Color::DarkGrey)),
        Some("LetsEncrypt") => ("LE".into(), None),
        Some(other) => (other.to_string(), None),
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
            certificate_type: Some("LetsEncrypt".into()),
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
    async fn unisrv_dev_domain_skips_dns_prompt() {
        let mut new_host = unprovisioned_host();
        new_host.host = "demo.unisrv.dev".into();
        let mut provisioned = provisioned_host(0, 90);
        provisioned.host = "demo.unisrv.dev".into();

        let mock = MockApiClient::logged_in()
            .with_claim_host(Ok(new_host))
            .with_request_host_cert(Ok(provisioned));

        let result = claim_with_confirm(&mock, "demo.unisrv.dev", || {
            panic!("DNS prompt should be skipped for unisrv.dev subdomains")
        })
        .await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.get_hosts_dns_config_calls, 0);
        assert_eq!(calls.request_host_cert_calls, vec![host_id()]);
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

    #[test]
    fn cert_in_lockout_handles_missing_expiry() {
        let host = unprovisioned_host();
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
        cert_type: Option<&str>,
        valid_until: Option<NaiveDateTime>,
        attached: bool,
        created_at: NaiveDateTime,
    ) -> HostResponse {
        HostResponse {
            id: Uuid::new_v4(),
            host: name.into(),
            user_id: user_id(),
            service_id: attached.then(Uuid::new_v4),
            certificate_type: cert_type.map(str::to_string),
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
                Some("LetsEncrypt"),
                Some(now + Duration::days(73)),
                true,
                now - Duration::days(3),
            ),
            host_with(
                "expiring.example.com",
                Some("LetsEncrypt"),
                Some(now + Duration::days(12)),
                false,
                now - Duration::hours(1),
            ),
            host_with(
                "expired.example.com",
                Some("LetsEncrypt"),
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

        let (text, color) = format_cert_type(Some("LetsEncrypt"));
        assert_eq!(text, "LE");
        assert_eq!(color, None);

        let (text, _) = format_cert_type(Some("SelfSigned"));
        assert_eq!(text, "SelfSigned");
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
