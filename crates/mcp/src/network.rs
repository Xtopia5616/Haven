use haven_common::types::NetworkPolicy;
use std::net::{IpAddr, SocketAddr};

/// Validate and resolve an MCP HTTP endpoint before a client is created.
/// Restricted mode pins the first validated public address in reqwest so a
/// later DNS answer cannot silently redirect the connection to a private host.
pub(crate) async fn build_http_client(
    raw_url: &str,
    policy: NetworkPolicy,
) -> anyhow::Result<(reqwest::Client, reqwest::Url)> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|error| anyhow::anyhow!("invalid MCP HTTP URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("MCP HTTP URL must use http or https");
    }
    if url.username() != "" || url.password().is_some() {
        anyhow::bail!("MCP HTTP URL must not contain credentials");
    }
    let addresses = match policy {
        NetworkPolicy::Deny => anyhow::bail!("MCP HTTP connection denied by network policy"),
        NetworkPolicy::Open => Vec::new(),
        NetworkPolicy::Restricted => resolve_public_addresses(&url).await?,
    };

    let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if matches!(policy, NetworkPolicy::Restricted) {
        // A system proxy can resolve the destination on Haven's behalf,
        // defeating the public-IP validation above.
        builder = builder.no_proxy();
    }
    if let (Some(host), Some(address)) = (url.host_str(), addresses.first().copied()) {
        builder = builder.resolve(host, address);
    }
    let client = builder
        .build()
        .map_err(|error| anyhow::anyhow!("failed to build MCP HTTP client: {error}"))?;
    Ok((client, url))
}

async fn resolve_public_addresses(url: &reqwest::Url) -> anyhow::Result<Vec<SocketAddr>> {
    let Some(host) = url.host_str() else {
        anyhow::bail!("MCP HTTP URL has no host");
    };
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("MCP HTTP URL has no known port"))?;
    let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| anyhow::anyhow!("MCP HTTP DNS lookup failed: {error}"))?
            .collect()
    };
    if addresses.is_empty() {
        anyhow::bail!("MCP HTTP host did not resolve");
    }
    if addresses
        .iter()
        .any(|address| is_blocked_address(address.ip()))
    {
        anyhow::bail!("MCP HTTP restricted policy rejects private or local address");
    }
    Ok(addresses)
}

fn is_blocked_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_multicast()
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_blocked_address;
    use haven_common::types::NetworkPolicy;
    use std::net::IpAddr;

    #[test]
    fn rejects_local_and_private_addresses() {
        for value in ["127.0.0.1", "10.0.0.1", "192.168.1.2", "::1", "fc00::1"] {
            assert!(is_blocked_address(value.parse::<IpAddr>().unwrap()));
        }
        assert!(!is_blocked_address("1.1.1.1".parse().unwrap()));
    }

    #[tokio::test]
    async fn restricted_http_client_rejects_loopback_before_connecting() {
        let result =
            super::build_http_client("http://127.0.0.1:1/mcp", NetworkPolicy::Restricted).await;
        assert!(result.is_err());
    }
}
