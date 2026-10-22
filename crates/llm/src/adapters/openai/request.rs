use super::prompt_cache_key::is_unsupported_prompt_cache_key_error;
use super::*;

impl OpenAiAdapter {
    #[cfg(test)]
    pub(super) fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<LlmToolDefinition>,
        stream: bool,
    ) -> OpenAiRequest {
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
    ) -> OpenAiRequest {
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
        tools: Vec<LlmToolDefinition>,
        stream: bool,
        web_search_mode: WebSearchMode,
        max_tokens: u32,
    ) -> OpenAiRequest {
        self.build_request_body_with_mode_and_max_tokens_shared(
            &messages,
            &tools,
            stream,
            web_search_mode,
            max_tokens,
        )
    }

    pub(super) fn build_request_body_with_mode_and_max_tokens_shared(
        &self,
        messages: &[CanonicalMessage],
        tools: &[LlmToolDefinition],
        stream: bool,
        web_search_mode: WebSearchMode,
        max_tokens: u32,
    ) -> OpenAiRequest {
        let web_search_mode =
            if crate::adapters::provider_builtin_web_search_available(&self.endpoint, self.style) {
                web_search_mode
            } else {
                WebSearchMode::Off
            };
        let tool_names = self.tool_name_map(messages, tools);
        let prompt_cache_key = self.prompt_cache_key(messages, tools, web_search_mode);
        let x_grok_conv_id = self.xai_conversation_id(messages);
        let converted_messages = Self::convert_messages_with_system_split(
            messages,
            self.requires_reasoning_echo(),
            self.endpoint
                .reasoning_echo_max_chars
                .unwrap_or(Self::MAX_REASONING_ECHO_CHARS),
        );
        let mut wire_messages = converted_messages.messages;
        for message in &mut wire_messages {
            if let Some(calls) = &mut message.tool_calls {
                for call in calls {
                    call.function.name = tool_names.to_provider(&call.function.name);
                }
            }
        }
        let cache_diagnostics = CacheDiagnostics::for_request(
            prompt_cache_key.is_some() || x_grok_conv_id.is_some(),
            converted_messages.cache_sections_split,
        )
        .with_provider(self.endpoint.provider.clone());
        let thinking_extras = chat_thinking_extras(&self.endpoint);
        let mut request_tools = if tools.is_empty() {
            Vec::new()
        } else {
            Self::convert_tools_ref_with_names(tools, &tool_names)
        };
        let has_function_tools = !tools.is_empty();
        let search_request = crate::adapters::chat_search_request(
            &self.endpoint,
            self.style,
            web_search_mode,
            has_function_tools,
        );
        if let Some(tool_type) = search_request.server_tool_type {
            request_tools.push(OpenAiTool::Server(OpenAiServerTool {
                tool_type: tool_type.to_string(),
            }));
        }
        let tool_choice = if search_request.force_server_tool {
            Some(serde_json::json!("required"))
        } else if has_function_tools {
            Some(serde_json::json!("auto"))
        } else {
            None
        };
        OpenAiRequest {
            model: self.endpoint.model_name.clone(),
            messages: wire_messages,
            max_tokens: Some(max_tokens),
            stream,
            tools: if request_tools.is_empty() {
                None
            } else {
                Some(request_tools)
            },
            tool_choice,
            stop: self.endpoint.stop.clone(),
            response_format: self.endpoint.response_format.clone(),
            reasoning_effort: thinking_extras.reasoning_effort,
            thinking: thinking_extras.thinking,
            stream_options: if stream {
                Some(StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
            search_parameters: search_request.search_parameters,
            web_search_options: search_request.web_search_options,
            enable_search: search_request.enable_search,
            include_tool_usage: search_request.include_tool_usage,
            search_options: search_request.dashscope_search_options,
            prompt_cache_key,
            x_grok_conv_id,
            cache_diagnostics,
        }
    }

    pub(super) fn append_guidance_to_request(&self, body: &mut OpenAiRequest, guidance: &str) {
        let message = CanonicalMessage::user_text(guidance);
        let mut wire = Self::convert_messages_with_system_split(
            std::slice::from_ref(&message),
            self.requires_reasoning_echo(),
            self.endpoint
                .reasoning_echo_max_chars
                .unwrap_or(Self::MAX_REASONING_ECHO_CHARS),
        )
        .messages;
        body.messages.append(&mut wire);
    }

    pub(super) async fn send_chat_request(
        &self,
        url: &str,
        body: &mut OpenAiRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        match self.send_chat_request_once(url, body, stream).await {
            Ok(response) => {
                if body.prompt_cache_key.is_some() {
                    self.prompt_cache_key_support.remember_success();
                }
                Ok(response)
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
                self.send_chat_request_once(url, body, stream).await
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn send_chat_request_once(
        &self,
        url: &str,
        body: &OpenAiRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let mut req = self.client.post(url).headers(self.build_headers()?);
        if let Some(conversation_id) = &body.x_grok_conv_id {
            req = req.header("x-grok-conv-id", conversation_id.as_str());
        }
        req = req.json(body);
        if stream {
            if let Some(timeout) = self.endpoint.timeout_streaming_secs {
                tracing::trace!(
                    "chat_stream_inner: {}s total streaming request timeout",
                    timeout
                );
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
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        let tool_names = self.tool_name_map(&messages, &tools);
        let mut body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            stream,
            self.web_search_mode,
            max_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let url = format!(
            "{}/chat/completions",
            self.endpoint.base_url.trim_end_matches('/')
        );

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
        let resp = self.send_chat_request(&url, &mut body, stream).await?;

        let txt = read_text_bounded(resp, MAX_JSON_RESPONSE_BYTES).await?;
        tracing::trace!("provider response body: {} bytes", txt.len());
        let json: OpenAiResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        let mut response = self.parse_openai_response(json, model, body.cache_diagnostics)?;
        for call in &mut response.tool_calls {
            call.name = tool_names.to_canonical(&call.name);
        }
        Ok(response)
    }
}
