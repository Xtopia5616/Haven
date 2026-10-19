use super::{McpClient, ResourceSubscription};
use crate::features::{MAX_RESOURCE_SUBSCRIPTIONS, validate_uri};
use crate::protocol::REQUEST_TIMEOUT_SECS;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

impl McpClient {
    pub(crate) async fn subscribe_resource(
        self: &Arc<Self>,
        uri: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<bool> {
        validate_uri(uri).map_err(anyhow::Error::msg)?;
        if !self.server_capabilities.lock().await.resource_subscribe {
            anyhow::bail!("MCP server does not advertise resource subscriptions");
        }

        {
            let subscriptions = self.resource_subscriptions.lock().await;
            if subscriptions.contains_key(uri) {
                return Ok(true);
            }
            if subscriptions.len() >= MAX_RESOURCE_SUBSCRIPTIONS {
                anyhow::bail!("MCP resource subscription limit reached");
            }
        }

        // Only resources presented by this server's current list can be
        // watched. This keeps the subscription command on the same
        // user-selected path as resources/read.
        let resources = self.list_resources(false, cancel.child_token()).await?;
        if !resources
            .resources
            .iter()
            .any(|resource| resource.uri == uri)
        {
            anyhow::bail!("resource must be selected from the current MCP resource list");
        }

        let mut subscriptions = self.resource_subscriptions.lock().await;
        if subscriptions.contains_key(uri) {
            return Ok(true);
        }
        if subscriptions.len() >= MAX_RESOURCE_SUBSCRIPTIONS {
            anyhow::bail!("MCP resource subscription limit reached");
        }
        if !self.server_capabilities.lock().await.resource_subscribe {
            anyhow::bail!("MCP server no longer advertises resource subscriptions");
        }

        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let subscription_cancel = self.cancel_token.lock().await.child_token();
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        self.resource_subscription_acks
            .lock()
            .await
            .insert(id, (uri.to_string(), ack_tx));

        let task = {
            let inner = self.inner.lock().await;
            let Some(inner) = inner.as_ref() else {
                self.resource_subscription_acks.lock().await.remove(&id);
                anyhow::bail!("MCP client is not connected");
            };
            inner.spawn_resource_subscription(
                self.name.clone(),
                id,
                uri.to_string(),
                subscription_cancel.clone(),
            )
        };
        subscriptions.insert(
            uri.to_string(),
            ResourceSubscription {
                subscription_id: id,
                cancel: subscription_cancel,
                task,
            },
        );
        drop(subscriptions);

        let acknowledged = tokio::select! {
            _ = cancel.cancelled() => {
                self.unsubscribe_resource(uri).await?;
                anyhow::bail!("MCP resource subscription cancelled");
            }
            result = tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), ack_rx) => {
                match result {
                    Ok(Ok(acknowledged)) => acknowledged,
                    Ok(Err(_)) => {
                        self.unsubscribe_resource(uri).await?;
                        anyhow::bail!("MCP resource subscription stream ended before acknowledgment");
                    }
                    Err(_) => {
                        self.unsubscribe_resource(uri).await?;
                        anyhow::bail!("MCP resource subscription acknowledgment timed out");
                    }
                }
            }
        };

        if !acknowledged {
            self.unsubscribe_resource(uri).await?;
        }
        Ok(acknowledged)
    }

    pub(crate) async fn unsubscribe_resource(&self, uri: &str) -> anyhow::Result<bool> {
        let Some(subscription) = self.resource_subscriptions.lock().await.remove(uri) else {
            return Ok(false);
        };
        subscription.cancel.cancel();
        self.resource_subscription_acks
            .lock()
            .await
            .remove(&subscription.subscription_id);
        let mut task = subscription.task;
        if tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
            tracing::warn!(server = %self.name, "MCP resource subscription did not stop after cancellation; aborting");
        }
        Ok(true)
    }

    pub(super) async fn stop_resource_subscriptions(&self) {
        let subscriptions = std::mem::take(&mut *self.resource_subscriptions.lock().await);
        self.resource_subscription_acks.lock().await.clear();
        for subscription in subscriptions.values() {
            subscription.cancel.cancel();
        }
        for subscription in subscriptions.into_values() {
            let mut task = subscription.task;
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
                tracing::warn!(server = %self.name, "MCP resource subscription did not stop during shutdown; aborting");
            }
        }
    }

    pub(super) async fn acknowledge_resource_subscription(&self, notification: &serde_json::Value) {
        let Some(subscription_id) =
            notification["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].as_u64()
        else {
            return;
        };
        let Some((requested_uri, sender)) = self
            .resource_subscription_acks
            .lock()
            .await
            .remove(&subscription_id)
        else {
            return;
        };
        let acknowledged = notification["params"]["notifications"]["resourceSubscriptions"]
            .as_array()
            .is_some_and(|uris| {
                uris.iter()
                    .any(|uri| uri.as_str() == Some(requested_uri.as_str()))
            });
        let _ = sender.send(acknowledged);
    }

    pub(super) async fn accept_resource_update(
        &self,
        notification: &serde_json::Value,
    ) -> Option<String> {
        let subscription_id =
            notification["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].as_u64()?;
        let uri = notification["params"]["uri"].as_str()?;
        let is_current = self
            .resource_subscriptions
            .lock()
            .await
            .get(uri)
            .is_some_and(|subscription| subscription.subscription_id == subscription_id);
        if !is_current {
            return None;
        }
        self.resource_read_cache.lock().await.remove(uri);
        Some(uri.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::McpResourceReadCacheEntry;
    use crate::features::{McpCacheScope, McpResourceRead, McpServerCapabilities};
    use std::time::Instant;

    async fn client_with_resource_subscriptions() -> Arc<McpClient> {
        let config = haven_common::config::McpServerConfig {
            name: "resource-subscription-test".into(),
            transport: haven_common::mcp::McpServerTransport::Http,
            url: "http://127.0.0.1:1/mcp".into(),
            ..Default::default()
        };
        let client = Arc::new(McpClient::new(&config, 1024, 1024));
        let capabilities = McpServerCapabilities::from_wire_capabilities(Some(
            &serde_json::json!({"resources": {"subscribe": true}}),
        ));
        *client.server_capabilities.lock().await = capabilities;
        client
    }

    #[tokio::test]
    async fn resource_subscription_acknowledgment_matches_the_requested_uri() {
        let client = client_with_resource_subscriptions().await;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        client
            .resource_subscription_acks
            .lock()
            .await
            .insert(41, ("mcp://server/item".into(), sender));

        client
            .acknowledge_resource_subscription(&serde_json::json!({
                "method": "notifications/subscriptions/acknowledged",
                "params": {
                    "_meta": {"io.modelcontextprotocol/subscriptionId": 41},
                    "notifications": {"resourceSubscriptions": ["mcp://server/other"]}
                }
            }))
            .await;

        assert!(!receiver.await.expect("acknowledgment result is delivered"));
    }

    #[tokio::test]
    async fn resource_update_is_bound_to_the_active_subscription_and_evicts_read_cache() {
        let client = client_with_resource_subscriptions().await;
        let uri = "mcp://server/item";
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move { task_cancel.cancelled().await });
        client.resource_subscriptions.lock().await.insert(
            uri.into(),
            ResourceSubscription {
                subscription_id: 41,
                cancel,
                task,
            },
        );
        client.resource_read_cache.lock().await.insert(
            uri.into(),
            McpResourceReadCacheEntry {
                result: McpResourceRead {
                    contents: Vec::new(),
                    ttl_ms: Some(30_000),
                    cache_scope: McpCacheScope::Private,
                },
                expires_at: Some(Instant::now() + Duration::from_secs(30)),
            },
        );

        let stale_update = serde_json::json!({
            "params": {
                "_meta": {"io.modelcontextprotocol/subscriptionId": 42},
                "uri": uri
            }
        });
        assert!(client.accept_resource_update(&stale_update).await.is_none());
        assert!(client.resource_read_cache.lock().await.contains_key(uri));

        let update = serde_json::json!({
            "params": {
                "_meta": {"io.modelcontextprotocol/subscriptionId": 41},
                "uri": uri
            }
        });
        assert_eq!(
            client.accept_resource_update(&update).await.as_deref(),
            Some(uri)
        );
        assert!(!client.resource_read_cache.lock().await.contains_key(uri));
        assert!(client.unsubscribe_resource(uri).await.unwrap());
    }

    #[tokio::test]
    async fn resource_subscription_count_is_bounded_before_network_access() {
        let client = client_with_resource_subscriptions().await;
        for index in 0..MAX_RESOURCE_SUBSCRIPTIONS {
            let uri = format!("mcp://server/{index}");
            let cancel = CancellationToken::new();
            let task_cancel = cancel.clone();
            let task = tokio::spawn(async move { task_cancel.cancelled().await });
            client.resource_subscriptions.lock().await.insert(
                uri,
                ResourceSubscription {
                    subscription_id: index as u64 + 1,
                    cancel,
                    task,
                },
            );
        }

        let error = client
            .subscribe_resource("mcp://server/new", CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("subscription limit"));
        client.stop_resource_subscriptions().await;
    }
}
