use super::*;

impl OpenAiResponsesAdapter {
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
        tools: Vec<ToolDefinition>,
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
        tools: Vec<ToolDefinition>,
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
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
        web_search_mode: WebSearchMode,
        max_output_tokens: u32,
    ) -> ResponsesRequest {
        // DeepSeek's official Responses contract ignores `prompt_cache_key`;
        // do not advertise an optional OpenAI extension to that endpoint.
        let prompt_cache_key = (!is_deepseek(&self.endpoint))
            .then(|| self.prompt_cache_key(&messages, &tools, web_search_mode))
            .flatten();
        let cache_diagnostics = Self::cache_diagnostics(&messages, prompt_cache_key.is_some());
        let max_reasoning_echo_chars = self
            .endpoint
            .reasoning_echo_max_chars
            .unwrap_or(Self::MAX_REASONING_ECHO_CHARS);
        let requires_reasoning_echo = self.requires_reasoning_echo();
        let (input, instructions) =
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
        let mut tools_json = Self::convert_tools(tools);
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
                tools_json.push(json!({"type": "web_search"}));
                Some(json!("auto"))
            }
            WebSearchMode::Always => {
                tools_json.push(json!({"type": "web_search"}));
                Some(json!({"type": "web_search"}))
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
            instructions,
            input,
            max_output_tokens: Some(max_output_tokens),
            temperature,
            top_p,
            stream,
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
                        let _ = self.prompt_cache_key_state.compare_exchange(
                            PROMPT_CACHE_KEY_UNKNOWN,
                            PROMPT_CACHE_KEY_ENABLED,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        );
                    }
                    return Ok(response);
                }
                Err(error)
                    if body.prompt_cache_key.is_some()
                        && Self::prompt_cache_key_rejected(&error) =>
                {
                    self.prompt_cache_key_state
                        .store(PROMPT_CACHE_KEY_UNSUPPORTED, Ordering::Relaxed);
                    body.prompt_cache_key = None;
                    body.cache_diagnostics.key_requested = false;
                    body.cache_diagnostics.downgraded = true;
                    body.cache_diagnostics.mode = if body.cache_diagnostics.system_split {
                        "split".into()
                    } else {
                        "off".into()
                    };
                    tracing::warn!(
                        endpoint = %crate::client::endpoint_log_location(url),
                        "endpoint rejected prompt_cache_key; disabled cache routing hint for this adapter"
                    );
                }
                Err(error)
                    if Self::developer_input_rejected(&error)
                        && Self::merge_developer_memory_into_instructions(body) =>
                {
                    self.developer_input_state
                        .store(DEVELOPER_INPUT_UNSUPPORTED, Ordering::Relaxed);
                    body.cache_diagnostics.system_split = false;
                    body.cache_diagnostics.downgraded = true;
                    body.cache_diagnostics.mode = if body.cache_diagnostics.key_requested {
                        "key".into()
                    } else {
                        "off".into()
                    };
                    tracing::warn!(
                        endpoint = %crate::client::endpoint_log_location(url),
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
            .headers(self.build_headers()?)
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
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, tools, stream, None)
            .await
    }

    pub(super) async fn chat_inner_with_max_tokens(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        let mut body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            stream,
            self.web_search_mode,
            max_output_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let url = self.responses_url();
        tracing::debug!(
            endpoint = %crate::client::endpoint_log_location(&url),
            model = %body.model,
            request_kind = "chat",
            "POST provider endpoint"
        );
        tracing::debug!(
            endpoint = %crate::client::endpoint_log_location(&url),
            "POST provider request body: {} chars",
            serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
        );
        let resp = self.send_request(&url, &mut body, stream).await?;

        let txt = read_text_bounded(resp, MAX_JSON_RESPONSE_BYTES).await?;
        tracing::trace!("provider response body: {} chars", txt.len());
        let json: ResponsesResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        self.parse_response_with_cache(json, model, body.cache_diagnostics)
    }
}
