use super::*;

impl OpenAiAdapter {
    #[cfg(test)]
    pub(super) fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
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
        tools: Vec<ToolDefinition>,
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
        tools: Vec<ToolDefinition>,
        stream: bool,
        web_search_mode: WebSearchMode,
        max_tokens: u32,
    ) -> OpenAiRequest {
        let has_tools = !tools.is_empty();
        let prompt_cache_key = self.prompt_cache_key(&messages, &tools);
        let (messages, system_split) = Self::split_system_memory(messages);
        let cache_diagnostics =
            CacheDiagnostics::for_request(prompt_cache_key.is_some(), system_split);
        let (thinking, reasoning_effort) = chat_thinking_extras(&self.endpoint);
        let omit_temperature = reasoning_effort.is_some() || thinking.is_some();
        // DeepSeek explicitly documents these sampling parameters as
        // unsupported in thinking mode. Omit them instead of relying on the
        // compatibility behavior that silently ignores them.
        let deepseek_thinking = is_deepseek(&self.endpoint)
            && thinking
                .as_ref()
                .and_then(|value| value.get("type"))
                .and_then(Value::as_str)
                == Some("enabled");
        let search_parameters = if self.style == "xai" {
            xai_search_mode(web_search_mode).map(|mode| {
                serde_json::json!({
                    "mode": mode,
                    "return_citations": true,
                })
            })
        } else {
            None
        };
        OpenAiRequest {
            model: self.endpoint.model_name.clone(),
            messages: Self::convert_messages(
                messages,
                self.requires_reasoning_echo(),
                self.endpoint
                    .reasoning_echo_max_chars
                    .unwrap_or(Self::MAX_REASONING_ECHO_CHARS),
            ),
            max_tokens: Some(max_tokens),
            // Reasoning / thinking modes reject or ignore non-default
            // temperature. Omit whenever effort or vendor thinking is pinned.
            temperature: (!omit_temperature).then_some(self.endpoint.temperature),
            stream,
            tools: if has_tools {
                Some(Self::convert_tools(tools))
            } else {
                None
            },
            tool_choice: if has_tools {
                Some(serde_json::json!("auto"))
            } else {
                None
            },
            top_p: (!deepseek_thinking)
                .then_some(self.endpoint.top_p)
                .flatten(),
            top_k: self.endpoint.top_k,
            frequency_penalty: (!deepseek_thinking)
                .then_some(self.endpoint.frequency_penalty)
                .flatten(),
            presence_penalty: (!deepseek_thinking)
                .then_some(self.endpoint.presence_penalty)
                .flatten(),
            stop: self.endpoint.stop.clone(),
            seed: self.endpoint.seed,
            response_format: self.endpoint.response_format.clone(),
            reasoning_effort,
            thinking,
            stream_options: if stream {
                Some(StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
            search_parameters,
            prompt_cache_key,
            cache_diagnostics,
        }
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
                    let _ = self.prompt_cache_key_state.compare_exchange(
                        PROMPT_CACHE_KEY_UNKNOWN,
                        PROMPT_CACHE_KEY_ENABLED,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
                Ok(response)
            }
            Err(error)
                if body.prompt_cache_key.is_some() && Self::prompt_cache_key_rejected(&error) =>
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
                    endpoint = %self.endpoint.base_url,
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
        let mut req = self
            .client
            .post(url)
            .headers(self.build_headers()?)
            .json(body);
        if stream {
            if let Some(timeout) = self.endpoint.timeout_streaming_secs {
                tracing::trace!("chat_stream_inner: {}s streaming timeout", timeout);
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
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
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

        tracing::debug!("POST {} (model: {})", url, body.model);
        tracing::debug!(
            "POST {} request body: {} chars",
            url,
            serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
        );
        let resp = self.send_chat_request(&url, &mut body, stream).await?;

        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        tracing::trace!("POST {} response body: {} chars", url, txt.len());
        let json: OpenAiResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        self.parse_openai_response(json, model, body.cache_diagnostics)
    }
}
