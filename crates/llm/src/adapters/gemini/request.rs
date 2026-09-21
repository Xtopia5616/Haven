use super::*;

impl GeminiAdapter {
    pub(super) fn api_base(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1beta") || base.ends_with("/v1") {
            base.to_string()
        } else {
            format!("{}/v1beta", base)
        }
    }

    pub(super) fn generate_url(&self) -> String {
        format!(
            "{}/models/{}:generateContent",
            self.api_base(),
            self.model_id()
        )
    }

    pub(super) fn stream_generate_url(&self) -> String {
        format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.api_base(),
            self.model_id()
        )
    }

    pub(super) fn model_id(&self) -> &str {
        self.endpoint.model_name.trim_start_matches("models/")
    }

    pub(super) fn models_url(&self) -> String {
        format!("{}/models", self.api_base())
    }

    pub(super) fn cached_contents_url(&self) -> String {
        format!("{}/cachedContents", self.api_base())
    }

    pub(super) fn embed_model_id(&self) -> &str {
        self.endpoint.model_name.trim_start_matches("models/")
    }

    pub(super) fn embed_url(&self) -> String {
        format!(
            "{}/models/{}:batchEmbedContents",
            self.api_base(),
            self.embed_model_id()
        )
    }

    #[cfg(test)]
    pub(super) fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        _stream: bool,
    ) -> GeminiRequest {
        self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            self.web_search_mode,
            self.endpoint.max_tokens,
        )
    }

    #[cfg(test)]
    pub(super) fn build_request_body_with_mode(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        web_search_mode: WebSearchMode,
    ) -> GeminiRequest {
        self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            web_search_mode,
            self.endpoint.max_tokens,
        )
    }

    pub(super) fn build_request_body_with_mode_and_max_tokens(
        &self,
        messages: impl AsRef<[CanonicalMessage]>,
        tools: impl AsRef<[ToolDefinition]>,
        web_search_mode: WebSearchMode,
        max_output_tokens: u32,
    ) -> GeminiRequest {
        let messages = messages.as_ref();
        let tools = tools.as_ref();
        let system_split = messages.iter().any(|message| {
            message.role == CanonicalRole::System
                && message.content.iter().any(|part| {
                    matches!(part, ContentPart::Text(text) if split_system_prompt_cache_sections(text).is_some())
                })
        });
        let (contents, system_instruction) = Self::convert_contents(messages);
        let mut tools_json = Self::convert_tools(tools);
        // Gemini grounding: append `{"google_search": {}}`. Auto and Always
        // both expose the tool (Gemini has no forced-search tool_choice
        // equivalent for google_search); Always still opts the model in.
        if !matches!(web_search_mode, WebSearchMode::Off) {
            tools_json.push(GeminiTool::GoogleSearch {
                google_search: json!({}),
            });
        }
        GeminiRequest {
            contents,
            system_instruction,
            cached_content: None,
            tools: if tools_json.is_empty() {
                None
            } else {
                Some(tools_json)
            },
            generation_config: Some(GeminiGenerationConfig {
                temperature: self.endpoint.temperature,
                max_output_tokens,
                top_p: self.endpoint.top_p,
                top_k: self.endpoint.top_k,
                stop_sequences: self.endpoint.stop.clone(),
            }),
            cache_diagnostics: CacheDiagnostics::for_provider_cache(system_split),
        }
    }

    /// Prepare an explicit Gemini context cache for the complete system
    /// instruction and tool surface. Gemini rejects a generate request that
    /// combines `cachedContent` with `systemInstruction` or `tools`, so the
    /// cache fingerprint includes both and the request removes both fields on
    /// success. If cache creation is unavailable (for example on a Gemini
    /// compatible gateway), the original split request remains unchanged.
    pub(super) async fn prepare_cached_content(&self, body: &mut GeminiRequest) {
        let Some(system_instruction) = body.system_instruction.clone() else {
            return;
        };
        let fingerprint = self.cached_content_fingerprint(&system_instruction, body.tools.as_ref());
        let mut state = self.cached_content.lock().await;
        let now = current_epoch_seconds();
        if let Some(entry) = state.entry.as_ref()
            && entry.fingerprint == fingerprint
        {
            Self::apply_cached_content(body, &entry.name);
            return;
        }
        if state
            .unavailable
            .as_ref()
            .is_some_and(|(key, retry_at)| key == &fingerprint && now < *retry_at)
        {
            return;
        }

        match self
            .create_cached_content(&system_instruction, body.tools.as_deref(), &fingerprint)
            .await
        {
            Ok(name) => {
                state.entry = Some(GeminiCacheEntry {
                    fingerprint,
                    name: name.clone(),
                });
                state.unavailable = None;
                Self::apply_cached_content(body, &name);
            }
            Err(error) => {
                state.unavailable =
                    Some((fingerprint, now.saturating_add(GEMINI_CACHE_RETRY_SECS)));
                tracing::debug!(
                    endpoint = %crate::client::endpoint_log_location(&self.cached_contents_url()),
                    error = %error,
                    "Gemini explicit context cache unavailable; using direct prompt"
                );
            }
        }
    }

    fn apply_cached_content(body: &mut GeminiRequest, name: &str) {
        body.cached_content = Some(name.to_string());
        body.system_instruction = None;
        body.tools = None;
        body.cache_diagnostics =
            CacheDiagnostics::for_explicit_provider_cache(body.cache_diagnostics.system_split);
    }

    async fn create_cached_content(
        &self,
        system_instruction: &Value,
        tools: Option<&[GeminiTool]>,
        fingerprint: &str,
    ) -> Result<String, LlmError> {
        let body = GeminiCachedContentCreateRequest {
            model: format!("models/{}", self.model_id()),
            system_instruction,
            tools,
            ttl: format!("{GEMINI_CACHE_TTL_SECS}s"),
            display_name: fingerprint.to_string(),
        };
        let req = self
            .client
            .post(self.cached_contents_url())
            .headers(self.build_headers()?)
            .json(&body)
            .timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let response = send_request(req, None).await?;
        let text = read_text_bounded(response, MAX_JSON_RESPONSE_BYTES).await?;
        let response: GeminiCachedContentResponse = serde_json::from_str(&text)
            .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;
        response
            .name
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| LlmError::InvalidResponse("Gemini cache response missing name".into()))
    }

    pub(super) async fn send_generate_request(
        &self,
        url: &str,
        body: &mut GeminiRequest,
        uncached_body: &GeminiRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        match self.send_generate_request_once(url, body, stream).await {
            Ok(response) => Ok(response),
            Err(error)
                if body.cached_content.is_some() && Self::cached_content_rejected(&error) =>
            {
                self.invalidate_cached_content(body.cached_content.as_deref())
                    .await;
                *body = uncached_body.clone();
                body.cache_diagnostics.downgraded = true;
                body.cache_diagnostics.mode = if body.cache_diagnostics.system_split {
                    "split".into()
                } else {
                    "implicit".into()
                };
                tracing::debug!(
                    endpoint = %crate::client::endpoint_log_location(url),
                    "Gemini cachedContent was rejected; retried with direct prompt"
                );
                self.send_generate_request_once(url, body, stream).await
            }
            Err(error) => Err(error),
        }
    }

    async fn send_generate_request_once(
        &self,
        url: &str,
        body: &GeminiRequest,
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

    async fn invalidate_cached_content(&self, name: Option<&str>) {
        let mut state = self.cached_content.lock().await;
        let Some(entry) = state.entry.take() else {
            return;
        };
        if name == Some(entry.name.as_str()) {
            state.unavailable = Some((
                entry.fingerprint,
                current_epoch_seconds().saturating_add(GEMINI_CACHE_RETRY_SECS),
            ));
        } else {
            state.entry = Some(entry);
        }
    }

    pub(super) fn append_guidance_to_request(&self, body: &mut GeminiRequest, guidance: &str) {
        let message = CanonicalMessage::user_text(guidance);
        let (mut contents, _) = Self::convert_contents(std::slice::from_ref(&message));
        body.contents.append(&mut contents);
    }
}

const GEMINI_CACHE_TTL_SECS: u64 = 3600;
const GEMINI_CACHE_RETRY_SECS: u64 = 300;
