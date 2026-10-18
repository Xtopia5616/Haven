use super::*;
use crate::adapters::openai::prompt_cache_key::is_unsupported_prompt_cache_key_error;

const CHATGPT_PLAN_TOOL_NAMESPACE: &str = "haven";

impl OpenAiResponsesAdapter {
    pub(super) fn tool_name_map(
        &self,
        messages: &[CanonicalMessage],
        tools: &[LlmToolDefinition],
    ) -> ToolNameMap {
        let max_len = if is_deepseek(&self.endpoint)
            || haven_common::config::is_chatgpt_plan_provider(&self.endpoint.provider)
        {
            128
        } else {
            64
        };
        ToolNameMap::for_request(tools, messages, ToolNamePolicy::Restricted { max_len })
    }

    pub(super) fn responses_url(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/responses", base)
        } else {
            format!("{}/v1/responses", base)
        }
    }

    #[cfg(test)]
    pub(super) fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<LlmToolDefinition>,
        stream: bool,
    ) -> ResponsesRequest {
        self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            stream,
            self.web_search_mode,
            self.endpoint.max_tokens,
        )
    }

    #[cfg(test)]
    pub(super) fn build_request_body_with_mode(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<LlmToolDefinition>,
        stream: bool,
        web_search_mode: WebSearchMode,
    ) -> ResponsesRequest {
        self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            stream,
            web_search_mode,
            self.endpoint.max_tokens,
        )
    }

    pub(super) fn build_request_body_with_mode_and_max_tokens(
        &self,
        messages: impl AsRef<[CanonicalMessage]>,
        tools: impl AsRef<[LlmToolDefinition]>,
        stream: bool,
        web_search_mode: WebSearchMode,
        max_output_tokens: u32,
    ) -> ResponsesRequest {
        let messages = messages.as_ref();
        let tools = tools.as_ref();
        let web_search_mode =
            if !haven_common::config::is_chatgpt_plan_provider(&self.endpoint.provider)
                && provider_builtin_web_search_available(&self.endpoint, "openai-responses")
            {
                web_search_mode
            } else {
                WebSearchMode::Off
            };
        let tool_names = self.tool_name_map(messages, tools);
        let is_chatgpt_plan =
            haven_common::config::is_chatgpt_plan_provider(&self.endpoint.provider);
        let prompt_cache_key = if is_chatgpt_plan {
            None
        } else {
            self.prompt_cache_key(messages, tools, web_search_mode)
        };
        let cache_diagnostics = Self::cache_diagnostics(messages, prompt_cache_key.is_some())
            .with_provider(self.endpoint.provider.clone());
        let max_reasoning_echo_chars = self
            .endpoint
            .reasoning_echo_max_chars
            .unwrap_or(Self::MAX_REASONING_ECHO_CHARS);
        let requires_reasoning_echo = self.requires_reasoning_echo();
        let mut converted_input =
            if self.developer_input_state.load(Ordering::Relaxed) == DEVELOPER_INPUT_UNSUPPORTED {
                Self::convert_input_with_memory_split(
                    messages,
                    max_reasoning_echo_chars,
                    requires_reasoning_echo,
                    false,
                )
            } else {
                Self::convert_input(messages, max_reasoning_echo_chars, requires_reasoning_echo)
            };
        for item in &mut converted_input.input {
            if item.get("type").and_then(Value::as_str) == Some("function_call")
                && let Some(name) = item.get("name").and_then(Value::as_str)
            {
                item["name"] = Value::String(tool_names.to_provider(name));
                if is_chatgpt_plan {
                    item["namespace"] = json!(CHATGPT_PLAN_TOOL_NAMESPACE);
                }
            }
        }
        let mut tools_json = Self::convert_tools_with_names(tools, &tool_names);
        if is_chatgpt_plan && !tools_json.is_empty() {
            tools_json = vec![json!({
                "type": "namespace",
                "name": CHATGPT_PLAN_TOOL_NAMESPACE,
                "description": "Haven local tools",
                "tools": tools_json
            })];
        }
        // `tool_choice` semantics: `None` (no tools at all), string
        // `"auto"`, or a specific tool object like
        // `{"type": "web_search"}` for forced search.
        let tool_choice: Option<Value> = match web_search_mode {
            WebSearchMode::Off => {
                if tools_json.is_empty() {
                    None
                } else {
                    Some(json!("auto"))
                }
            }
            WebSearchMode::Auto => {
                tools_json.push(json!({"type": responses_web_search_tool_type(&self.endpoint)}));
                Some(json!("auto"))
            }
            WebSearchMode::Always => {
                let tool_type = responses_web_search_tool_type(&self.endpoint);
                tools_json.push(json!({"type": tool_type}));
                if crate::adapters::responses_search_can_force(&self.endpoint) {
                    Some(json!({"type": tool_type}))
                } else {
                    Some(json!("auto"))
                }
            }
        };
        let reasoning = responses_reasoning_config(&self.endpoint);
        let temperature = if reasoning.is_some() || self.endpoint.temperature == 1.0 {
            None
        } else {
            Some(self.endpoint.temperature)
        };
        let top_p = if reasoning.is_some() {
            None
        } else {
            self.endpoint.top_p
        };
        let text = self.endpoint.response_format.clone().map(|format| {
            if format.get("format").is_some() {
                format
            } else {
                json!({"format": format})
            }
        });
        ResponsesRequest {
            model: self.endpoint.model_name.clone(),
            instructions: converted_input.instructions,
            input: converted_input.input,
            max_output_tokens: (!is_chatgpt_plan).then_some(max_output_tokens),
            temperature: (!is_chatgpt_plan).then_some(temperature).flatten(),
            top_p: (!is_chatgpt_plan).then_some(top_p).flatten(),
            stream: stream || is_chatgpt_plan,
            store: is_chatgpt_plan.then_some(false),
            tools: if tools_json.is_empty() {
                None
            } else {
                Some(tools_json)
            },
            tool_choice,
            reasoning,
            output_config: responses_output_config(&self.endpoint),
            text,
            prompt_cache_key,
            cache_diagnostics,
        }
    }

    pub(super) fn append_guidance_to_request(&self, body: &mut ResponsesRequest, guidance: &str) {
        let message = CanonicalMessage::user_text(guidance);
        let mut converted_input =
            if self.developer_input_state.load(Ordering::Relaxed) == DEVELOPER_INPUT_UNSUPPORTED {
                Self::convert_input_with_memory_split(
                    std::slice::from_ref(&message),
                    self.endpoint
                        .reasoning_echo_max_chars
                        .unwrap_or(Self::MAX_REASONING_ECHO_CHARS),
                    self.requires_reasoning_echo(),
                    false,
                )
            } else {
                Self::convert_input(
                    std::slice::from_ref(&message),
                    self.endpoint
                        .reasoning_echo_max_chars
                        .unwrap_or(Self::MAX_REASONING_ECHO_CHARS),
                    self.requires_reasoning_echo(),
                )
            };
        body.input.append(&mut converted_input.input);
    }

    pub(super) async fn send_request(
        &self,
        url: &str,
        body: &mut ResponsesRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        // At most one retry for each optional cache extension. A gateway can
        // reject both fields independently, so keep trying after either safe
        // downgrade instead of making their order observable to callers.
        for _ in 0..=2 {
            match self.send_request_once(url, body, stream).await {
                Ok(response) => {
                    if body.prompt_cache_key.is_some() {
                        self.prompt_cache_key_support.remember_success();
                    }
                    return Ok(response);
                }
                Err(error)
                    if body.prompt_cache_key.is_some()
                        && is_unsupported_prompt_cache_key_error(&error) =>
                {
                    self.prompt_cache_key_support.remember_rejection();
                    body.prompt_cache_key = None;
                    body.cache_diagnostics.key_requested = false;
                    body.cache_diagnostics.downgraded = true;
                    body.cache_diagnostics.strategy = if body.cache_diagnostics.system_split {
                        haven_common::usage::PromptCacheStrategy::Split
                    } else {
                        haven_common::usage::PromptCacheStrategy::Off
                    };
                    tracing::warn!(
                        endpoint = %crate::client::endpoint_host(url),
                        "endpoint rejected prompt_cache_key; disabled cache routing hint for this adapter"
                    );
                }
                Err(error)
                    if Self::developer_input_rejected(&error)
                        && Self::downgrade_developer_input(body) =>
                {
                    self.developer_input_state
                        .store(DEVELOPER_INPUT_UNSUPPORTED, Ordering::Relaxed);
                    body.cache_diagnostics.system_split = false;
                    body.cache_diagnostics.downgraded = true;
                    body.cache_diagnostics.strategy = if body.cache_diagnostics.key_requested {
                        haven_common::usage::PromptCacheStrategy::Key
                    } else {
                        haven_common::usage::PromptCacheStrategy::Off
                    };
                    tracing::warn!(
                        endpoint = %crate::client::endpoint_host(url),
                        "endpoint rejected developer input; disabled Responses memory split for this adapter"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("each Responses cache downgrade removes a rejected request feature")
    }

    pub(super) async fn send_request_once(
        &self,
        url: &str,
        body: &ResponsesRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let mut req = self
            .client
            .post(url)
            .headers(self.request_headers().await?)
            .json(body);
        if stream {
            if let Some(timeout) = self.endpoint.timeout_streaming_secs {
                req = req.timeout(Duration::from_secs(timeout));
            }
        } else {
            req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        }
        send_request(
            req,
            if stream {
                stream_header_timeout(self.endpoint.timeout_streaming_secs)
            } else {
                None
            },
        )
        .await
    }

    pub(super) async fn chat_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<LlmToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, tools, stream, None)
            .await
    }

    pub(super) async fn chat_inner_with_max_tokens(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<LlmToolDefinition>,
        stream: bool,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        if haven_common::config::is_chatgpt_plan_provider(&self.endpoint.provider) {
            use futures_util::StreamExt;

            let mut chunks = self
                .chat_stream_inner_with_max_tokens(messages, tools, max_output_tokens)
                .await?;
            let mut response = LlmResponse {
                model: Some(self.endpoint.model_name.clone()),
                ..Default::default()
            };
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk?;
                if let Some(text) = chunk.text {
                    response.text.push_str(&text);
                }
                if !chunk.tool_calls.is_empty() {
                    response.tool_calls = chunk.tool_calls;
                }
                if let Some(reasoning) = chunk.reasoning {
                    response
                        .reasoning
                        .get_or_insert_with(String::new)
                        .push_str(&reasoning);
                }
                if let Some(usage) = chunk.usage {
                    response.usage = usage;
                }
                if chunk.finish_reason.is_some() {
                    response.finish_reason = chunk.finish_reason;
                }
                if let Some(model) = chunk.model {
                    response.model = Some(model);
                }
                response.web_search_calls.extend(chunk.web_search_calls);
                response.thinking_blocks.extend(chunk.thinking_blocks);
            }
            return Ok(response);
        }
        let tool_names = self.tool_name_map(&messages, &tools);
        let mut body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            stream,
            self.web_search_mode,
            max_output_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let url = self.responses_url();
        tracing::debug!(
            endpoint = %crate::client::endpoint_host(&url),
            model = %body.model,
            request_kind = "chat",
            "POST provider endpoint"
        );
        tracing::debug!(
            endpoint = %crate::client::endpoint_host(&url),
            "POST provider request body: {} bytes",
            serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
        );
        let resp = self.send_request(&url, &mut body, stream).await?;

        let txt = read_text_bounded(resp, MAX_JSON_RESPONSE_BYTES).await?;
        tracing::trace!("provider response body: {} bytes", txt.len());
        let json: ResponsesResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        let mut response = self.parse_response_with_cache(json, model, body.cache_diagnostics)?;
        for call in &mut response.tool_calls {
            call.name = tool_names.to_canonical(&call.name);
        }
        Ok(response)
    }
}
