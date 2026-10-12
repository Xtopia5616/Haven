//! Shared HTTP destination validation for requests that follow untrusted URLs.
//!
//! Callers must disable automatic redirects and resolve each hop through this
//! module before sending it. Hostname resolution rejects mixed public/private
//! answers and returns a pinned address so DNS cannot change between checking
//! and connecting.

use std::net::{IpAddr, SocketAddr};

use url::Url;

pub const MAX_HTTP_REDIRECT_HOPS: usize = 10;
pub const MAX_HTTP_URL_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHttpDestination {
    pub url: Url,
    /// DNS names are pinned to the first validated public address. Literal IP
    /// destinations need no resolver override because the URL already fixes
    /// the address.
    pub pinned_address: Option<SocketAddr>,
}

/// Parse an HTTP(S) destination, reject local or special-use hosts, and pin a
/// validated public DNS result. An allowlist is an additional restriction; an
/// empty list permits any public host.
pub async fn resolve_http_destination(
    raw_url: &str,
    allowed_domains: &[String],
) -> anyhow::Result<ResolvedHttpDestination> {
    resolve_http_destination_inner(raw_url, allowed_domains, false).await
}

/// Test-only loopback access for local HTTP fixtures. This is compiled only
/// when a consumer explicitly enables the `test-support` feature.
#[cfg(feature = "test-support")]
pub async fn resolve_http_destination_for_test(
    raw_url: &str,
    allowed_domains: &[String],
) -> anyhow::Result<ResolvedHttpDestination> {
    resolve_http_destination_inner(raw_url, allowed_domains, true).await
}

async fn resolve_http_destination_inner(
    raw_url: &str,
    allowed_domains: &[String],
    allow_loopback: bool,
) -> anyhow::Result<ResolvedHttpDestination> {
    if raw_url.len() > MAX_HTTP_URL_BYTES {
        anyhow::bail!("HTTP destination exceeds the {MAX_HTTP_URL_BYTES} byte limit");
    }
    let url = Url::parse(raw_url).map_err(|error| anyhow::anyhow!("invalid HTTP URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("HTTP destination must use http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("HTTP destination must not contain credentials");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("HTTP destination must include a host"))?;
    let host = normalize_http_host(host);
    if is_blocked_metadata_host(&host) && !(allow_loopback && host == "localhost") {
        anyhow::bail!("HTTP destination is a blocked local or metadata host");
    }
    if !http_domain_allowed(&host, allowed_domains) {
        anyhow::bail!("HTTP destination is not in the configured domain allowlist");
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("HTTP destination has no supported port"))?;

    let (pinned_address, addresses) = if let Ok(ip) = host.parse::<IpAddr>() {
        (None, vec![SocketAddr::new(ip, port)])
    } else {
        let addresses = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|error| anyhow::anyhow!("failed to resolve HTTP destination: {error}"))?
            .collect::<Vec<_>>();
        let pinned_address = addresses.first().copied();
        (pinned_address, addresses)
    };

    if addresses.is_empty() {
        anyhow::bail!("HTTP destination did not resolve to an address");
    }
    if addresses
        .iter()
        .any(|address| is_blocked_ip(address.ip(), allow_loopback))
    {
        anyhow::bail!("HTTP destination resolves to a blocked local or private address");
    }

    Ok(ResolvedHttpDestination {
        url,
        pinned_address,
    })
}

pub fn normalize_http_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

pub fn http_domain_allowed(host: &str, allowed_domains: &[String]) -> bool {
    if allowed_domains.is_empty() {
        return true;
    }
    allowed_domains.iter().any(|entry| {
        let entry = normalize_http_host(entry.trim());
        if let Some(suffix) = entry.strip_prefix("*.") {
            host.ends_with(&format!(".{suffix}")) && host != suffix
        } else {
            host == entry
        }
    })
}

fn is_blocked_metadata_host(host: &str) -> bool {
    matches!(
        host,
        "localhost"
            | "metadata"
            | "metadata.google.internal"
            | "metadata.azure.internal"
            | "metadata.internal"
            | "instance-data"
            | "instance-data.ec2.internal"
    )
}

fn is_blocked_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    if allow_loopback && ip.is_loopback() {
        return false;
    }
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            let first = octets[0];
            let second = octets[1];
            ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_multicast()
                || first == 0
                || (first == 100 && (64..=127).contains(&second))
                || (first == 192 && second == 0 && octets[2] == 2)
                || (first == 192 && second == 0 && octets[2] == 0)
                || (first == 198 && (18..=19).contains(&second))
                || (first == 198 && second == 51 && octets[2] == 100)
                || (first == 203 && second == 0 && octets[2] == 113)
                || first >= 240
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(mapped), allow_loopback);
            }
            let segments = ip.segments();
            ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (segments[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation 2001:db8::/32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_special_use_ipv4_and_ipv6_addresses() {
        for value in [
            "0.0.0.0",
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "192.168.1.2",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_blocked_ip(value.parse().unwrap(), false), "{value}");
        }
        for value in ["1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!is_blocked_ip(value.parse().unwrap(), false), "{value}");
        }
    }

    #[tokio::test]
    async fn rejects_local_metadata_credentials_and_non_http_destinations_before_dns() {
        for url in [
            "http://127.0.0.1/latest/meta-data/",
            "http://192.168.1.2/",
            "http://metadata.google.internal/",
            "https://user:password@example.com/",
            "file:///C:/Windows/win.ini",
        ] {
            assert!(resolve_http_destination(url, &[]).await.is_err(), "{url}");
        }
    }

    #[tokio::test]
    async fn rejects_hosts_outside_allowlist_before_dns() {
        let allowed = vec!["example.com".to_string()];
        assert!(
            resolve_http_destination("https://not-allowed.invalid/", &allowed)
                .await
                .is_err()
        );
    }

    #[test]
    fn http_domain_allowlist_matches_exact_hosts_and_subdomains() {
        assert!(http_domain_allowed("example.com", &[]));
        assert!(http_domain_allowed("example.com", &["EXAMPLE.COM.".into()]));
        assert!(http_domain_allowed(
            "api.example.com",
            &["*.example.com".into()]
        ));
        assert!(!http_domain_allowed(
            "example.com",
            &["*.example.com".into()]
        ));
        assert!(!http_domain_allowed(
            "badexample.com",
            &["*.example.com".into()]
        ));
    }
}
