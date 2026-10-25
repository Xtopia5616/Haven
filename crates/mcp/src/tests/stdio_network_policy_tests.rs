use super::*;
use haven_common::types::NetworkPolicy;

struct RejectingProcessAuthorization;

impl crate::McpStdioProcessStartAuthorization for RejectingProcessAuthorization {
    fn acquire_process_start_permit(
        &self,
        _request: crate::McpStdioProcessStartRequest,
    ) -> crate::McpStdioProcessStartPermitFuture<'_> {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "process authorization reached the stdio startup boundary"
            ))
        })
    }
}

#[tokio::test]
async fn stdio_policy_decision_is_delegated_to_the_process_authorization() {
    for policy in [NetworkPolicy::Restricted, NetworkPolicy::Deny] {
        let config = McpServerConfig {
            name: format!("stdio-policy-{policy:?}"),
            transport: haven_common::mcp::McpServerTransport::Stdio,
            command: "must-not-spawn".into(),
            ..Default::default()
        };
        let client = McpClient::new_managed_with_launcher(
            &config,
            1024,
            1024,
            Some(Arc::new(RejectingProcessAuthorization)),
            crate::client::local_stdio_process_launcher(),
        );
        client.set_network_policy(policy).await;

        let error = tokio::time::timeout(Duration::from_secs(3), client.connect())
            .await
            .expect("startup authorization must be bounded")
            .expect_err("the injected process authorization rejects startup");

        assert!(
            error
                .to_string()
                .contains("process authorization reached the stdio startup boundary"),
            "{policy:?} must be decided by the process authorization, got {error}"
        );
        assert!(matches!(
            client.status().await,
            McpClientStatus::Offline { .. }
        ));
    }
}

#[tokio::test]
async fn stdio_without_a_tools_authorization_snapshot_fails_closed() {
    let config = McpServerConfig {
        name: "stdio-missing-process-authorization".into(),
        transport: haven_common::mcp::McpServerTransport::Stdio,
        command: "must-not-spawn".into(),
        ..Default::default()
    };
    let client = crate::client::McpClient::new_managed_with_launcher(
        &config,
        1024,
        1024,
        None,
        crate::client::local_stdio_process_launcher(),
    );
    client.set_network_policy(NetworkPolicy::Open).await;

    let error = tokio::time::timeout(Duration::from_secs(3), client.connect())
        .await
        .expect("startup authorization must be bounded")
        .expect_err("a stdio process must not spawn without Tools authorization");

    assert!(error.to_string().contains("no process-start authorization"));
}
