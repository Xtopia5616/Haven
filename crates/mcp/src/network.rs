use haven_common::types::NetworkPolicy;

pub(crate) struct BuiltMcpHttpClient {
    pub(crate) client: reqwest::Client,
    pub(crate) endpoint: reqwest::Url,
}

/// Validate and resolve an MCP HTTP endpoint before a client is created.
/// Restricted mode pins the first validated public address in reqwest so a
/// later DNS answer cannot silently redirect the connection to a private host.
/// Approval prompts are owned by the authorization gateway, separately from
/// this destination boundary.
pub(crate) async fn build_http_client(
    raw_url: &str,
    policy: NetworkPolicy,
) -> anyhow::Result<BuiltMcpHttpClient> {
    let (url, pinned_address) = match policy {
        NetworkPolicy::Deny => anyhow::bail!("MCP HTTP connection denied by network policy"),
        NetworkPolicy::Open => {
            let url = parse_open_endpoint(raw_url)?;
            (url, None)
        }
        NetworkPolicy::Restricted => {
            let destination = haven_common::network::resolve_http_destination(raw_url, &[]).await?;
            (destination.url, destination.pinned_address)
        }
    };

    let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if matches!(policy, NetworkPolicy::Restricted) {
        // A system proxy can resolve the destination on Haven's behalf,
        // defeating the public-IP validation above.
        builder = builder.no_proxy();
    }
    if let (Some(host), Some(address)) = (url.host_str(), pinned_address) {
        builder = builder.resolve(host, address);
    }
    let client = builder
        .build()
        .map_err(|error| anyhow::anyhow!("failed to build MCP HTTP client: {error}"))?;
    Ok(BuiltMcpHttpClient {
        client,
        endpoint: url,
    })
}

fn parse_open_endpoint(raw_url: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|error| anyhow::anyhow!("invalid MCP HTTP URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("MCP HTTP URL must use http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("MCP HTTP URL must not contain credentials");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::build_http_client;
    use haven_common::types::NetworkPolicy;

    #[tokio::test]
    async fn restricted_http_client_rejects_loopback_before_connecting() {
        let result = build_http_client("http://127.0.0.1:1/mcp", NetworkPolicy::Restricted).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn restricted_http_client_rejects_ipv4_mapped_loopback_before_connecting() {
        let result =
            build_http_client("http://[::ffff:127.0.0.1]:1/mcp", NetworkPolicy::Restricted).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn restricted_http_client_rejects_special_use_ipv4_before_connecting() {
        let result = build_http_client("http://100.64.0.1:1/mcp", NetworkPolicy::Restricted).await;
        assert!(result.is_err());
    }
}
