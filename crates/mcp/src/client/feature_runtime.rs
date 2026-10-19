use super::{
    McpClient, McpPromptListSnapshot, McpResourceListSnapshot, McpResourceReadCacheEntry,
    McpResourceTemplateListSnapshot,
};
use crate::features::{
    MAX_FEATURE_CACHE_TTL, MAX_FEATURE_LIST_BYTES, MAX_FEATURE_LIST_ITEMS, MAX_FEATURE_LIST_PAGES,
    MAX_FEATURE_NAME_BYTES, MAX_MCP_MRTR_ROUNDS, MAX_PENDING_MCP_FEATURE_INPUTS,
    MAX_PROMPT_ARGUMENT_BYTES, McpElicitationAnswer, McpElicitationRequest, McpFeatureCallOutcome,
    McpFeatureCallStatus, McpPrompt, McpPromptList, McpPromptListPage, McpPromptResponse,
    McpResourceList, McpResourceListPage, McpResourceRead, McpResourceReadResponse,
    McpResourceTemplateList, McpResourceTemplateListPage, parse_input_required,
    validate_complete_result, validate_cursor, validate_identity, validate_input_responses,
    validate_prompt, validate_prompt_arguments, validate_prompt_info, validate_resource_contents,
    validate_resource_info, validate_resource_template, validate_uri,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub(super) enum PendingFeatureOperation {
    ResourceRead { uri: String },
    PromptGet,
}

#[derive(Debug, Clone)]
pub(super) struct PendingFeatureRequest {
    pub(super) method: String,
    pub(super) params: Value,
    pub(super) operation: PendingFeatureOperation,
    pub(super) request_state: Option<String>,
    pub(super) requests: Vec<McpElicitationRequest>,
    pub(super) round: u32,
    pub(super) cancel: CancellationToken,
    pub(super) in_flight: bool,
}

impl McpClient {
    pub(crate) async fn list_resource_templates(
        &self,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceTemplateList> {
        if !self.server_capabilities.lock().await.resources {
            anyhow::bail!("MCP server does not advertise resources capability");
        }
        if !force_refresh
            && let Some(snapshot) = self.resource_template_list_cache.lock().await.as_ref()
            && snapshot
                .expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
        {
            return Ok(snapshot.result.clone());
        }

        let _refresh = self.resource_template_list_refresh.lock().await;
        if force_refresh {
            *self.resource_template_list_cache.lock().await = None;
        }
        if !force_refresh
            && let Some(snapshot) = self.resource_template_list_cache.lock().await.as_ref()
            && snapshot
                .expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
        {
            return Ok(snapshot.result.clone());
        }

        let snapshot = self.fetch_resource_template_list(cancel).await?;
        let result = snapshot.result.clone();
        *self.resource_template_list_cache.lock().await = Some(snapshot);
        Ok(result)
    }

    async fn fetch_resource_template_list(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceTemplateListSnapshot> {
        let fetched_at = Instant::now();
        let mut resource_templates = Vec::new();
        let mut cursor = None::<String>;
        let mut seen_cursors = HashSet::new();
        let mut templates = HashSet::new();
        let mut aggregate_bytes = 0usize;
        let mut pages = 0usize;
        let mut ttl_ms = None::<u64>;
        let mut cache_scope = crate::features::McpCacheScope::Public;
        loop {
            pages += 1;
            if pages > MAX_FEATURE_LIST_PAGES {
                anyhow::bail!("MCP resources/templates/list exceeded the page limit");
            }
            let params = cursor
                .as_ref()
                .map(|value| serde_json::json!({"cursor": value}));
            let result = self
                .request_with_cancel("resources/templates/list", params, &cancel)
                .await?;
            validate_complete_result(&result, "resources/templates/list")
                .map_err(anyhow::Error::msg)?;
            let page: McpResourceTemplateListPage =
                serde_json::from_value(result).map_err(|_| {
                    anyhow::anyhow!("MCP resources/templates/list returned an invalid page")
                })?;
            let page_ttl = page.ttl_ms.unwrap_or(0);
            ttl_ms = Some(ttl_ms.map_or(page_ttl, |current| current.min(page_ttl)));
            if crate::features::McpCacheScope::from_hint(page.cache_scope.as_deref())
                == crate::features::McpCacheScope::Private
            {
                cache_scope = crate::features::McpCacheScope::Private;
            }
            if resource_templates
                .len()
                .saturating_add(page.resource_templates.len())
                > MAX_FEATURE_LIST_ITEMS
            {
                anyhow::bail!("MCP resources/templates/list exceeded the item limit");
            }
            aggregate_bytes = aggregate_bytes.saturating_add(
                serde_json::to_vec(&page.resource_templates)
                    .map_err(|_| {
                        anyhow::anyhow!("MCP resources/templates/list page could not be measured")
                    })?
                    .len(),
            );
            if aggregate_bytes > MAX_FEATURE_LIST_BYTES {
                anyhow::bail!("MCP resources/templates/list exceeded the aggregate byte limit");
            }
            for template in &page.resource_templates {
                validate_resource_template(template).map_err(anyhow::Error::msg)?;
                if !templates.insert(template.uri_template.clone()) {
                    anyhow::bail!("MCP resources/templates/list returned duplicate URI templates");
                }
            }
            resource_templates.extend(page.resource_templates);
            cursor = page.next_cursor;
            validate_cursor(cursor.as_deref()).map_err(anyhow::Error::msg)?;
            let Some(next_cursor) = cursor.as_ref() else {
                break;
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                anyhow::bail!("MCP resources/templates/list repeated a pagination cursor");
            }
        }

        let expires_at =
            ttl_ms.map(|ttl| fetched_at + Duration::from_millis(ttl).min(MAX_FEATURE_CACHE_TTL));
        Ok(McpResourceTemplateListSnapshot {
            result: McpResourceTemplateList {
                resource_templates,
                ttl_ms,
                cache_scope,
            },
            expires_at,
        })
    }

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

    pub(crate) async fn read_resource_with_mrtr(
        &self,
        uri: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpFeatureCallOutcome> {
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
            return Ok(complete_resource_read(entry.result.clone()));
        }

        let params = serde_json::json!({"uri": uri});
        let pending_cancel = self.cancel_token.lock().await.child_token();
        self.start_feature_request(
            "resources/read",
            params,
            PendingFeatureOperation::ResourceRead {
                uri: uri.to_string(),
            },
            pending_cancel,
            cancel,
        )
        .await
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

    pub(crate) async fn get_prompt_with_mrtr(
        &self,
        name: &str,
        arguments: HashMap<String, String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpFeatureCallOutcome> {
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

        let params = serde_json::json!({"name": name, "arguments": arguments});
        let pending_cancel = self.cancel_token.lock().await.child_token();
        self.start_feature_request(
            "prompts/get",
            params,
            PendingFeatureOperation::PromptGet,
            pending_cancel,
            cancel,
        )
        .await
    }

    pub(crate) async fn respond_to_feature_input(
        &self,
        request_id: u64,
        answers: HashMap<String, McpElicitationAnswer>,
    ) -> anyhow::Result<McpFeatureCallOutcome> {
        let pending = {
            let mut interactions = self.pending_feature_inputs.lock().await;
            let Some(pending) = interactions.get_mut(&request_id) else {
                anyhow::bail!("MCP input request is no longer active");
            };
            if pending.in_flight {
                anyhow::bail!("MCP input request is already being submitted");
            }
            pending.in_flight = true;
            pending.clone()
        };

        let input_responses = if pending.requests.is_empty() {
            if !answers.is_empty() || pending.request_state.is_none() {
                if let Some(current) = self
                    .pending_feature_inputs
                    .lock()
                    .await
                    .get_mut(&request_id)
                {
                    current.in_flight = false;
                }
                anyhow::bail!("MCP state-only retry does not accept user form answers");
            }
            None
        } else {
            match validate_input_responses(&pending.requests, &answers) {
                Ok(responses) => Some(responses),
                Err(error) => {
                    if let Some(current) = self
                        .pending_feature_inputs
                        .lock()
                        .await
                        .get_mut(&request_id)
                    {
                        current.in_flight = false;
                    }
                    return Err(anyhow::Error::msg(error));
                }
            }
        };
        let mut params = pending.params.clone();
        let params_object = params
            .as_object_mut()
            .expect("feature request parameters are stored as an object");
        if let Some(input_responses) = input_responses {
            params_object.insert("inputResponses".into(), input_responses);
        }
        if let Some(request_state) = &pending.request_state {
            params_object.insert("requestState".into(), Value::String(request_state.clone()));
        }
        let request = self
            .request_with_client_capabilities(
                &pending.method,
                Some(params),
                form_elicitation_capabilities(),
                &pending.cancel,
            )
            .await;
        let (_, result) = match request {
            Ok(result) => result,
            Err(error) => {
                self.pending_feature_inputs.lock().await.remove(&request_id);
                pending.cancel.cancel();
                return Err(error);
            }
        };
        let outcome = self
            .process_feature_result(
                request_id,
                result,
                pending.method.clone(),
                pending.params.clone(),
                pending.operation.clone(),
                pending.round,
                pending.request_state.clone(),
                pending.cancel.clone(),
            )
            .await;
        if outcome.is_err() {
            self.pending_feature_inputs.lock().await.remove(&request_id);
            pending.cancel.cancel();
        }
        outcome
    }

    pub(crate) async fn cancel_feature_input(&self, request_id: u64) -> bool {
        let Some(pending) = self.pending_feature_inputs.lock().await.remove(&request_id) else {
            return false;
        };
        pending.cancel.cancel();
        true
    }

    async fn start_feature_request(
        &self,
        method: &str,
        params: Value,
        operation: PendingFeatureOperation,
        pending_cancel: CancellationToken,
        caller_cancel: CancellationToken,
    ) -> anyhow::Result<McpFeatureCallOutcome> {
        let (pending_id, result) = self
            .request_with_client_capabilities(
                method,
                Some(params.clone()),
                form_elicitation_capabilities(),
                &caller_cancel,
            )
            .await?;
        self.process_feature_result(
            pending_id,
            result,
            method.to_string(),
            params,
            operation,
            0,
            None,
            pending_cancel,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_feature_result(
        &self,
        pending_id: u64,
        result: Value,
        method: String,
        base_params: Value,
        operation: PendingFeatureOperation,
        mut round: u32,
        mut previous_state: Option<String>,
        pending_cancel: CancellationToken,
    ) -> anyhow::Result<McpFeatureCallOutcome> {
        loop {
            if result.get("resultType").and_then(Value::as_str) == Some("input_required") {
                round = round.saturating_add(1);
                if round > MAX_MCP_MRTR_ROUNDS {
                    anyhow::bail!("MCP input_required exceeded the round limit");
                }
                let parsed = parse_input_required(&result).map_err(anyhow::Error::msg)?;
                if parsed.request_state.is_some() {
                    previous_state = parsed.request_state;
                }
                let is_resuming = self
                    .pending_feature_inputs
                    .lock()
                    .await
                    .contains_key(&pending_id);
                let mut interactions = self.pending_feature_inputs.lock().await;
                if pending_cancel.is_cancelled() {
                    anyhow::bail!("MCP input request was cancelled");
                }
                if !is_resuming && interactions.len() >= MAX_PENDING_MCP_FEATURE_INPUTS {
                    anyhow::bail!("Haven has too many MCP input requests awaiting the user");
                }
                interactions.insert(
                    pending_id,
                    PendingFeatureRequest {
                        method,
                        params: base_params,
                        operation,
                        request_state: previous_state,
                        requests: parsed.requests.clone(),
                        round,
                        cancel: pending_cancel,
                        in_flight: false,
                    },
                );
                return Ok(McpFeatureCallOutcome {
                    status: McpFeatureCallStatus::InputRequired,
                    resource_read: None,
                    prompt: None,
                    request_id: Some(pending_id),
                    round: Some(round),
                    requests: parsed.requests,
                });
            }

            validate_complete_result(&result, &method).map_err(anyhow::Error::msg)?;
            self.pending_feature_inputs.lock().await.remove(&pending_id);
            return match operation {
                PendingFeatureOperation::ResourceRead { uri } => {
                    let read = self.parse_and_cache_resource_read(&uri, result).await?;
                    Ok(complete_resource_read(read))
                }
                PendingFeatureOperation::PromptGet => {
                    let prompt = parse_prompt_result(result)?;
                    Ok(complete_prompt(prompt))
                }
            };
        }
    }

    async fn parse_and_cache_resource_read(
        &self,
        uri: &str,
        result: Value,
    ) -> anyhow::Result<McpResourceRead> {
        let fetched_at = Instant::now();
        let response: McpResourceReadResponse = serde_json::from_value(result)
            .map_err(|_| anyhow::anyhow!("MCP resources/read returned an invalid response"))?;
        validate_resource_contents(&response.contents).map_err(anyhow::Error::msg)?;
        let read = McpResourceRead {
            contents: response.contents,
            ttl_ms: response.ttl_ms,
            cache_scope: crate::features::McpCacheScope::from_hint(response.cache_scope.as_deref()),
        };
        if let Some(ttl_ms) = read.ttl_ms {
            let expires_at = fetched_at + Duration::from_millis(ttl_ms).min(MAX_FEATURE_CACHE_TTL);
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

    async fn request_with_cancel(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<Value> {
        self.request_with_client_capabilities(method, params, serde_json::json!({}), cancel)
            .await
            .map(|(_, result)| result)
    }

    async fn request_with_client_capabilities(
        &self,
        method: &str,
        params: Option<Value>,
        client_capabilities: Value,
        cancel: &CancellationToken,
    ) -> anyhow::Result<(u64, Value)> {
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
            result = guard.as_mut().expect("checked MCP client inner").request_with_client_capabilities(
                id, method, params, &[], client_capabilities,
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
            Ok(result) => Ok((id, result)),
            Err(error) => {
                self.discard_interrupted_stdio(&mut guard).await;
                Err(error)
            }
        }
    }
}

fn form_elicitation_capabilities() -> Value {
    serde_json::json!({"elicitation": {"form": {}}})
}

fn complete_resource_read(read: McpResourceRead) -> McpFeatureCallOutcome {
    McpFeatureCallOutcome {
        status: McpFeatureCallStatus::Complete,
        resource_read: Some(read),
        prompt: None,
        request_id: None,
        round: None,
        requests: Vec::new(),
    }
}

fn complete_prompt(prompt: McpPrompt) -> McpFeatureCallOutcome {
    McpFeatureCallOutcome {
        status: McpFeatureCallStatus::Complete,
        resource_read: None,
        prompt: Some(prompt),
        request_id: None,
        round: None,
        requests: Vec::new(),
    }
}

fn parse_prompt_result(result: Value) -> anyhow::Result<McpPrompt> {
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
