use super::{McpClient, McpPromptListSnapshot, McpResourceListSnapshot, McpResourceReadCacheEntry};
use crate::features::{
    MAX_FEATURE_CACHE_TTL, MAX_FEATURE_LIST_BYTES, MAX_FEATURE_LIST_ITEMS, MAX_FEATURE_LIST_PAGES,
    MAX_FEATURE_NAME_BYTES, MAX_PROMPT_ARGUMENT_BYTES, McpPrompt, McpPromptList, McpPromptListPage,
    McpPromptResponse, McpResourceList, McpResourceListPage, McpResourceRead,
    McpResourceReadResponse, validate_complete_result, validate_cursor, validate_identity,
    validate_prompt, validate_prompt_arguments, validate_prompt_info, validate_resource_contents,
    validate_resource_info, validate_uri,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

impl McpClient {
    pub(crate) async fn list_resources(
        &self,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceList> {
        if !self.server_capabilities.lock().await.resources {
            anyhow::bail!("MCP server does not advertise resources capability");
        }
        if !force_refresh
            && let Some(snapshot) = self.resource_list_cache.lock().await.as_ref()
            && snapshot
                .expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
        {
            return Ok(snapshot.result.clone());
        }

        let _refresh = self.resource_list_refresh.lock().await;
        if force_refresh {
            *self.resource_list_cache.lock().await = None;
        }
        if !force_refresh
            && let Some(snapshot) = self.resource_list_cache.lock().await.as_ref()
            && snapshot
                .expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
        {
            return Ok(snapshot.result.clone());
        }
        let snapshot = self.fetch_resource_list(cancel).await?;
        let result = snapshot.result.clone();
        *self.resource_list_cache.lock().await = Some(snapshot);
        Ok(result)
    }

    async fn fetch_resource_list(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceListSnapshot> {
        let fetched_at = Instant::now();
        let mut resources = Vec::new();
        let mut cursor = None::<String>;
        let mut seen_cursors = HashSet::new();
        let mut names = HashSet::new();
        let mut aggregate_bytes = 0usize;
        let mut pages = 0usize;
        let mut ttl_ms = None::<u64>;
        let mut cache_scope = crate::features::McpCacheScope::Public;
        loop {
            pages += 1;
            if pages > MAX_FEATURE_LIST_PAGES {
                anyhow::bail!("MCP resources/list exceeded the page limit");
            }
            let params = cursor
                .as_ref()
                .map(|value| serde_json::json!({"cursor": value}));
            let result = self
                .request_with_cancel("resources/list", params, &cancel)
                .await?;
            validate_complete_result(&result, "resources/list").map_err(anyhow::Error::msg)?;
            let page: McpResourceListPage = serde_json::from_value(result)
                .map_err(|_| anyhow::anyhow!("MCP resources/list returned an invalid page"))?;
            let page_ttl = page.ttl_ms.unwrap_or(0);
            ttl_ms = Some(ttl_ms.map_or(page_ttl, |current| current.min(page_ttl)));
            if crate::features::McpCacheScope::from_hint(page.cache_scope.as_deref())
                == crate::features::McpCacheScope::Private
            {
                cache_scope = crate::features::McpCacheScope::Private;
            }
            if resources.len().saturating_add(page.resources.len()) > MAX_FEATURE_LIST_ITEMS {
                anyhow::bail!("MCP resources/list exceeded the item limit");
            }
            aggregate_bytes = aggregate_bytes.saturating_add(
                serde_json::to_vec(&page.resources)
                    .map_err(|_| anyhow::anyhow!("MCP resources/list page could not be measured"))?
                    .len(),
            );
            if aggregate_bytes > MAX_FEATURE_LIST_BYTES {
                anyhow::bail!("MCP resources/list exceeded the aggregate byte limit");
            }
            for resource in &page.resources {
                validate_resource_info(resource).map_err(anyhow::Error::msg)?;
                if !names.insert(resource.uri.clone()) {
                    anyhow::bail!("MCP resources/list returned duplicate resource URIs");
                }
            }
            resources.extend(page.resources);
            cursor = page.next_cursor;
            validate_cursor(cursor.as_deref()).map_err(anyhow::Error::msg)?;
            let Some(next_cursor) = cursor.as_ref() else {
                break;
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                anyhow::bail!("MCP resources/list repeated a pagination cursor");
            }
        }
        let expires_at =
            ttl_ms.map(|ttl| fetched_at + Duration::from_millis(ttl).min(MAX_FEATURE_CACHE_TTL));
        Ok(McpResourceListSnapshot {
            result: McpResourceList {
                resources,
                ttl_ms,
                cache_scope,
            },
            expires_at,
        })
    }

    pub(crate) async fn read_resource(
        &self,
        uri: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceRead> {
        if !self.server_capabilities.lock().await.resources {
            anyhow::bail!("MCP server does not advertise resources capability");
        }
        validate_uri(uri).map_err(anyhow::Error::msg)?;
        if force_refresh {
            self.resource_read_cache.lock().await.remove(uri);
        }
        if !force_refresh
            && let Some(entry) = self.resource_read_cache.lock().await.get(uri)
            && entry
                .expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
        {
            return Ok(entry.result.clone());
        }

        let fetched_at = Instant::now();
        let result = self
            .request_with_cancel(
                "resources/read",
                Some(serde_json::json!({"uri": uri})),
                &cancel,
            )
            .await?;
        validate_complete_result(&result, "resources/read").map_err(anyhow::Error::msg)?;
        let response: McpResourceReadResponse = serde_json::from_value(result)
            .map_err(|_| anyhow::anyhow!("MCP resources/read returned an invalid response"))?;
        validate_resource_contents(&response.contents).map_err(anyhow::Error::msg)?;
        let cache_scope =
            crate::features::McpCacheScope::from_hint(response.cache_scope.as_deref());
        let expires_at = response
            .ttl_ms
            .map(|ttl| fetched_at + Duration::from_millis(ttl).min(MAX_FEATURE_CACHE_TTL));
        let read = McpResourceRead {
            contents: response.contents,
            ttl_ms: response.ttl_ms,
            cache_scope,
        };
        if let Some(expires_at) = expires_at {
            let mut cache = self.resource_read_cache.lock().await;
            if cache.len() >= 64 && !cache.contains_key(uri) {
                cache.clear();
            }
            cache.insert(
                uri.to_string(),
                McpResourceReadCacheEntry {
                    result: read.clone(),
                    expires_at: Some(expires_at),
                },
            );
        } else {
            self.resource_read_cache.lock().await.remove(uri);
        }
        Ok(read)
    }

    pub(crate) async fn list_prompts(
        &self,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpPromptList> {
        if !self.server_capabilities.lock().await.prompts {
            anyhow::bail!("MCP server does not advertise prompts capability");
        }
        if !force_refresh
            && let Some(snapshot) = self.prompt_list_cache.lock().await.as_ref()
            && snapshot
                .expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
        {
            return Ok(snapshot.result.clone());
        }

        let _refresh = self.prompt_list_refresh.lock().await;
        if force_refresh {
            *self.prompt_list_cache.lock().await = None;
        }
        if !force_refresh
            && let Some(snapshot) = self.prompt_list_cache.lock().await.as_ref()
            && snapshot
                .expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
        {
            return Ok(snapshot.result.clone());
        }
        let snapshot = self.fetch_prompt_list(cancel).await?;
        let result = snapshot.result.clone();
        *self.prompt_list_cache.lock().await = Some(snapshot);
        Ok(result)
    }

    async fn fetch_prompt_list(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpPromptListSnapshot> {
        let fetched_at = Instant::now();
        let mut prompts = Vec::new();
        let mut cursor = None::<String>;
        let mut seen_cursors = HashSet::new();
        let mut names = HashSet::new();
        let mut aggregate_bytes = 0usize;
        let mut pages = 0usize;
        let mut ttl_ms = None::<u64>;
        let mut cache_scope = crate::features::McpCacheScope::Public;
        loop {
            pages += 1;
            if pages > MAX_FEATURE_LIST_PAGES {
                anyhow::bail!("MCP prompts/list exceeded the page limit");
            }
            let params = cursor
                .as_ref()
                .map(|value| serde_json::json!({"cursor": value}));
            let result = self
                .request_with_cancel("prompts/list", params, &cancel)
                .await?;
            validate_complete_result(&result, "prompts/list").map_err(anyhow::Error::msg)?;
            let page: McpPromptListPage = serde_json::from_value(result)
                .map_err(|_| anyhow::anyhow!("MCP prompts/list returned an invalid page"))?;
            let page_ttl = page.ttl_ms.unwrap_or(0);
            ttl_ms = Some(ttl_ms.map_or(page_ttl, |current| current.min(page_ttl)));
            if crate::features::McpCacheScope::from_hint(page.cache_scope.as_deref())
                == crate::features::McpCacheScope::Private
            {
                cache_scope = crate::features::McpCacheScope::Private;
            }
            if prompts.len().saturating_add(page.prompts.len()) > MAX_FEATURE_LIST_ITEMS {
                anyhow::bail!("MCP prompts/list exceeded the item limit");
            }
            aggregate_bytes = aggregate_bytes.saturating_add(
                serde_json::to_vec(&page.prompts)
                    .map_err(|_| anyhow::anyhow!("MCP prompts/list page could not be measured"))?
                    .len(),
            );
            if aggregate_bytes > MAX_FEATURE_LIST_BYTES {
                anyhow::bail!("MCP prompts/list exceeded the aggregate byte limit");
            }
            for prompt in &page.prompts {
                validate_prompt_info(prompt).map_err(anyhow::Error::msg)?;
                if !names.insert(prompt.name.clone()) {
                    anyhow::bail!("MCP prompts/list returned duplicate prompt names");
                }
            }
            prompts.extend(page.prompts);
            cursor = page.next_cursor;
            validate_cursor(cursor.as_deref()).map_err(anyhow::Error::msg)?;
            let Some(next_cursor) = cursor.as_ref() else {
                break;
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                anyhow::bail!("MCP prompts/list repeated a pagination cursor");
            }
        }
        let expires_at =
            ttl_ms.map(|ttl| fetched_at + Duration::from_millis(ttl).min(MAX_FEATURE_CACHE_TTL));
        Ok(McpPromptListSnapshot {
            result: McpPromptList {
                prompts,
                ttl_ms,
                cache_scope,
            },
            expires_at,
        })
    }

    pub(crate) async fn get_prompt(
        &self,
        name: &str,
        arguments: HashMap<String, String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpPrompt> {
        if !self.server_capabilities.lock().await.prompts {
            anyhow::bail!("MCP server does not advertise prompts capability");
        }
        validate_identity("prompt name", name, MAX_FEATURE_NAME_BYTES)
            .map_err(anyhow::Error::msg)?;
        let prompts = self.list_prompts(false, cancel.child_token()).await?;
        let prompt_info = prompts
            .prompts
            .iter()
            .find(|prompt| prompt.name == name)
            .ok_or_else(|| {
                anyhow::anyhow!("MCP prompt is not present in the current prompt catalog")
            })?;
        validate_prompt_arguments(prompt_info, &arguments).map_err(anyhow::Error::msg)?;
        if arguments
            .values()
            .any(|value| value.len() > MAX_PROMPT_ARGUMENT_BYTES)
        {
            anyhow::bail!("MCP prompt argument value exceeded the byte limit");
        }

        let result = self
            .request_with_cancel(
                "prompts/get",
                Some(serde_json::json!({"name": name, "arguments": arguments})),
                &cancel,
            )
            .await?;
        validate_complete_result(&result, "prompts/get").map_err(anyhow::Error::msg)?;
        let response: McpPromptResponse = serde_json::from_value(result.clone())
            .map_err(|_| anyhow::anyhow!("MCP prompts/get returned an invalid response"))?;
        let prompt = McpPrompt {
            description: response.description,
            messages: response.messages,
            ttl_ms: result.get("ttlMs").and_then(Value::as_u64),
            cache_scope: crate::features::McpCacheScope::from_hint(
                result.get("cacheScope").and_then(Value::as_str),
            ),
        };
        validate_prompt(&prompt).map_err(anyhow::Error::msg)?;
        Ok(prompt)
    }

    async fn request_with_cancel(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<Value> {
        let client_cancel = self.cancel_token.lock().await.clone();
        loop {
            let wait = {
                let mut rate_limiter = self.rate_limiter.lock().await;
                if rate_limiter.acquire() {
                    Duration::ZERO
                } else {
                    rate_limiter.wait_duration()
                }
            };
            if wait.is_zero() {
                break;
            }
            tokio::select! {
                _ = client_cancel.cancelled() => anyhow::bail!("MCP request cancelled (client shutting down)"),
                _ = cancel.cancelled() => anyhow::bail!("MCP request cancelled (caller cancelled)"),
                _ = tokio::time::sleep(wait) => {}
            }
        }

        let mut guard = tokio::select! {
            biased;
            _ = client_cancel.cancelled() => anyhow::bail!("MCP request cancelled (client shutting down)"),
            _ = cancel.cancelled() => anyhow::bail!("MCP request cancelled (caller cancelled)"),
            guard = self.inner.lock() => guard,
        };
        if guard.is_none() {
            anyhow::bail!("MCP client '{}' is not connected", self.name);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let result = tokio::select! {
            result = guard.as_mut().expect("checked MCP client inner").request(
                id, method, params, &[],
            ) => result,
            _ = client_cancel.cancelled() => {
                self.discard_interrupted_stdio(&mut guard).await;
                anyhow::bail!("MCP request cancelled (client shutting down)");
            }
            _ = cancel.cancelled() => {
                self.discard_interrupted_stdio(&mut guard).await;
                anyhow::bail!("MCP request cancelled (caller cancelled)");
            }
        };
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                self.discard_interrupted_stdio(&mut guard).await;
                Err(error)
            }
        }
    }
}
