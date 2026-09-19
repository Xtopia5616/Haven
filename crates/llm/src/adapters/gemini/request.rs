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
}
