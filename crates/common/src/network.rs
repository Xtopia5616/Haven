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
    if http_host_is_local_or_special(&host) && !(allow_loopback && host == "localhost") {
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

/// Whether a literal or well-known metadata host is local or special-use.
/// DNS names that resolve to private addresses are checked after resolution
/// by [`resolve_http_destination`].
pub fn http_host_is_local_or_special(host: &str) -> bool {
    let host = normalize_http_host(host);
    is_blocked_metadata_host(&host)
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| is_blocked_ip(address, false))
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
                || (first == 192 && second == 0 && octets[2] == 0 && !matches!(octets[3], 9 | 10))
                || (first == 192 && second == 88 && octets[2] == 99)
                || (first == 198 && (18..=19).contains(&second))
                || (first == 198 && second == 51 && octets[2] == 100)
                || (first == 203 && second == 0 && octets[2] == 113)
                || first >= 240
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                // Preserve the explicit loopback exception for mapped local
                // addresses while treating all other mapped addresses as a
                // special-use range.
                return !(allow_loopback && mapped.is_loopback());
            }
            let segments = ip.segments();
            let global_unicast = (segments[0] & 0xe000) == 0x2000; // 2000::/3
            let in_ietf_protocol_assignments = segments[0] == 0x2001 && segments[1] <= 0x01ff;
            let globally_reachable_ietf_exception = is_globally_reachable_ietf_assignment(segments);
            let well_known_nat64 = segments[0] == 0x0064
                && segments[1] == 0xff9b
                && segments[2..6].iter().all(|segment| *segment == 0);
            let nat64_embedded_ipv4 = well_known_nat64.then(|| {
                let octets = ip.octets();
                std::net::Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15])
            });
            ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || (!global_unicast && !well_known_nat64)
                || (segments[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (segments[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation 2001:db8::/32
                || (in_ietf_protocol_assignments && !globally_reachable_ietf_exception)
                || segments[0] == 0x2002 // 6to4 2002::/16
                || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0) // documentation 3fff::/20
                || segments[0] == 0x5f00 // SRv6 SIDs 5f00::/16
                || nat64_embedded_ipv4.is_some_and(|embedded| {
                    is_blocked_ip(IpAddr::V4(embedded), allow_loopback)
                })
        }
    }
}

fn is_globally_reachable_ietf_assignment(segments: [u16; 8]) -> bool {
    (segments[1] == 1
        && segments[2..7].iter().all(|segment| *segment == 0)
        && matches!(segments[7], 1..=3))
        || segments[1] == 3 // AMT 2001:3::/32
        || (segments[1] == 4 && segments[2] == 0x0112) // AS112 2001:4:112::/48
        || (0x0020..=0x002f).contains(&segments[1]) // ORCHIDv2 2001:20::/28
        || (0x0030..=0x003f).contains(&segments[1]) // Drone Remote ID 2001:30::/28
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
            "192.88.99.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:8.8.8.8",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "100::1",
            "100:0:0:1::1",
            "2001::1",
            "2001:2::1",
            "2002::1",
            "3fff::1",
            "5f00::1",
        ] {
            assert!(is_blocked_ip(value.parse().unwrap(), false), "{value}");
        }
        for value in [
            "1.1.1.1",
            "192.0.0.9",
            "192.0.0.10",
            "2001:1::1",
            "2001:1::2",
            "2001:1::3",
            "2001:3::1",
            "2001:4:112::1",
            "2001:20::1",
            "2001:2f:ffff::1",
            "2001:30::1",
            "2001:3f:ffff::1",
            "64:ff9b::808:808",
            "2606:4700:4700::1111",
        ] {
            assert!(!is_blocked_ip(value.parse().unwrap(), false), "{value}");
        }
        assert!(!is_blocked_ip("::ffff:127.0.0.1".parse().unwrap(), true));
        assert!(is_blocked_ip("::ffff:8.8.8.8".parse().unwrap(), true));
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
