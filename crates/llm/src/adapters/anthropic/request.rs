use super::*;

impl AnthropicAdapter {
    pub(super) fn messages_url(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/messages", base)
        } else {
            format!("{}/v1/messages", base)
        }
    }

    pub(super) fn models_url(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/models", base)
        } else {
            format!("{}/v1/models", base)
        }
    }

    #[cfg(test)]
    pub(super) fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> AnthropicRequest {
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
    ) -> AnthropicRequest {
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
        tools: impl AsRef<[ToolDefinition]>,
        stream: bool,
        web_search_mode: WebSearchMode,
        max_tokens: u32,
    ) -> AnthropicRequest {
        let messages = messages.as_ref();
        let tools = tools.as_ref();
        let cache_diagnostics = Self::cache_diagnostics(messages);
        let (messages, system) = Self::convert_messages(messages);
        let mut tools_json = Self::convert_tools(tools);
        let had_client_tools = !tools_json.is_empty();
        let tool_choice: Option<Value> = match web_search_mode {
            WebSearchMode::Off => {
                if tools_json.is_empty() {
                    None
                } else {
                    Some(json!({"type": "auto"}))
                }
            }
            WebSearchMode::Auto => {
                tools_json.push(json!({
                    "type": ANTHROPIC_WEB_SEARCH_TOOL_TYPE,
                    "name": "web_search",
                    "max_uses": 5,
                }));
                Some(json!({"type": "auto"}))
            }
            WebSearchMode::Always => {
                tools_json.push(json!({
                    "type": ANTHROPIC_WEB_SEARCH_TOOL_TYPE,
                    "name": "web_search",
                    "max_uses": 5,
                }));
                // Force only when there are no Haven ReAct function tools —
                // otherwise `tool_choice: web_search` blocks the agent loop.
                if had_client_tools {
                    Some(json!({"type": "auto"}))
                } else {
                    Some(json!({"type": "tool", "name": "web_search"}))
                }
            }
        };
        if !tools_json.is_empty() {
            Self::apply_tools_cache_breakpoint(&mut tools_json);
        }
        let mut messages = messages;
        Self::apply_messages_cache_breakpoint(&mut messages);
        let (thinking, output_config) = Self::thinking_config(
            max_tokens,
            &self.endpoint.model_name,
            self.endpoint.reasoning_effort.as_deref(),
        );
        let thinking_active = thinking
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "adaptive" | "enabled"));
        AnthropicRequest {
            model: self.endpoint.model_name.clone(),
            max_tokens,
            messages,
            system: Self::system_with_cache_control(system),
            temperature: (!thinking_active).then_some(self.endpoint.temperature),
            top_p: self.endpoint.top_p,
            top_k: self.endpoint.top_k,
            stop_sequences: self.endpoint.stop.clone(),
            tools: if tools_json.is_empty() {
                None
            } else {
                Some(tools_json)
            },
            tool_choice,
            thinking,
            output_config,
            stream,
            cache_diagnostics,
        }
    }

    pub(super) fn append_guidance_to_request(&self, body: &mut AnthropicRequest, guidance: &str) {
        let message = CanonicalMessage::user_text(guidance);
        let (mut wire, _) = Self::convert_messages(std::slice::from_ref(&message));
        body.messages.append(&mut wire);
    }
}
