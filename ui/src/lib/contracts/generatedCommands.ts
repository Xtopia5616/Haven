// Generated from #[tauri::command] handler signatures by `scripts/generate-ipc-contracts.ps1`.
// Do not edit by hand; `scripts/check-ipc-contracts.ps1` rejects drift.

// DTO declarations below are generated from Rust Serialize types.

export interface CounterSnapshot { turn_starts: number; first_tokens: number; stream_chunks: number; chunk_drops: number; checkpoint_pending: number; branch_point_failures: number; snapshot_failures: number; projection_failures: number; inbox_ack_failures: number; action_result_retries: number; action_result_duplicates: number; web_search_drops: number }
export interface GaugeSnapshot { context_queue_items: number }
export interface MetricsSnapshot { phases: PhaseSnapshot[]; counters: CounterSnapshot; gauges: GaugeSnapshot; ui?: UiMetricsSnapshot }
export interface PhaseSnapshot { count: number; total_ms: number; p50_ms: number; p95_ms: number }
export interface UiMetricsSnapshotInput { frames: number; chunks: number; drops: number }
export interface UiMetricsSnapshot { frames: number; chunks: number; drops: number }
export interface SessionInfo { id: string; input: string; summary: string; title: string | null; status: SessionStatus; waiting_reason?: SessionWaitingReason; steps: StepInfo[]; created_at: string; updated_at: string }
export interface StepInfo { id: string; step_number: number; tool_name: string; input: unknown; output: unknown | null; status: string; risk_level: RiskLevel; confirmed: boolean | null }
export type ProcessResult = { 'SessionCreated': { session_id: string; message_id?: string | null } } | { 'Supplemented': { message_id?: string | null } };
export interface SessionListResponse { sessions: SessionInfo[] }
export interface McpToolCallResponse { success: boolean; output: unknown; error: string | null }
export interface MemoryRecallItem { entity_id: string; text: string; score: number; model: string }
export interface SkillExecutionResponse { success: boolean; output: unknown; error: string | null }
export interface ToolListResponse { tools: ToolManifest[] }
export interface LogInfo { enabled: boolean; level: string; path: string | null }
export interface LogTail { path: string; content: string }
export interface McpRefreshResult { added: string[]; removed: string[]; updated: string[]; failed: string[] }
export interface ApiKeyStatus { models: Record<string, boolean>; providers: Record<string, boolean>; stt: boolean; ocr: boolean; ocr_secret: boolean }
export interface RecordingState { is_recording: boolean; is_toggle: boolean }
export interface SessionResumeResponse { session: Session; messages: Message[]; steps: SessionStep[]; usage: SessionUsage | null; llm_usage: LlmCallUsage[]; interactions: InteractionRequestedEvent[] }
export interface SessionPermissionGrant { session_id: string; session_title: string | null; capability: string; target: string; effect: string }
export interface ShellAvailability { available: boolean }
export interface ActionEvent { id: string; kind: ActionKind; status?: ActionStatus; session_id?: string; source_step_id?: string; started_at?: string; finished_at?: string; due_at?: string; title?: string; body?: string; mode?: string; command?: string; output?: string; error?: string; error_reason?: string; exit_code?: number; preview?: string }
export type ActionKindInput = 'background' | 'scheduled';
export type ActionKind = 'background' | 'scheduled';
export interface InteractionRequestedEvent { id: string; session_id: string; kind: string; status: string; options?: string[]; tool_name?: string; risk_level?: RiskLevel; summary?: string; permission_key?: string; invocation_step_id?: string; action_index?: number; tool_call_id?: string; created_at: string; expires_at?: string }
export type CapabilityInput = 'chat' | 'fast_chat' | 'vision' | 'audio_input' | 'transcription' | 'embedding' | 'image_generation' | 'speech_synthesis';
export type Capability = 'chat' | 'fast_chat' | 'vision' | 'audio_input' | 'transcription' | 'embedding' | 'image_generation' | 'speech_synthesis';
export interface LlmConfigInput { providers?: ProviderConfigInput[]; models?: ModelConfigInput[]; request_policies?: RequestPolicyInput[]; max_total_duration_secs?: number; stream_idle_timeout_secs?: number; retry_max_retries?: number; retry_base_secs?: number; retry_factor?: number; retry_max_secs?: number; retry_jitter?: number; max_concurrent_requests?: number }
export interface LlmConfig { providers: ProviderConfig[]; models: ModelConfig[]; request_policies: RequestPolicy[]; max_total_duration_secs: number; stream_idle_timeout_secs: number; retry_max_retries: number; retry_base_secs: number; retry_factor: number; retry_max_secs: number; retry_jitter: number; max_concurrent_requests: number }
export interface ModelConfigInput { id?: string; provider?: string; model?: string; capabilities?: CapabilityInput[]; temperature?: number | null; context_window?: number | null; cost_per_1k_input_tokens?: number | null; cost_per_1k_output_tokens?: number | null; cost_per_1k_cache_read_tokens?: number | null; cost_per_1k_cache_write_tokens?: number | null; max_tokens?: number | null; reasoning_effort?: string | null; web_search?: string | null; reasoning_echo_max_chars?: number | null }
export interface ModelConfig { id: string; provider: string; model: string; capabilities: Capability[]; temperature?: number; context_window?: number; cost_per_1k_input_tokens?: number; cost_per_1k_output_tokens?: number; cost_per_1k_cache_read_tokens?: number; cost_per_1k_cache_write_tokens?: number; max_tokens?: number; reasoning_effort?: string; web_search?: string; reasoning_echo_max_chars?: number }
export interface ProviderConfigInput { name?: string; provider?: string; api_style?: string | null; base_url?: string; api_key?: string; api_key_ref?: string | null; auth_header_name?: string; auth_header_prefix?: string; proxy_url?: string | null; no_proxy?: string | null; default_max_tokens?: number | null; default_temperature?: number | null; default_timeout_secs?: number | null; default_timeout_streaming_secs?: number | null; default_web_search?: string | null }
export interface ProviderConfig { name: string; provider: string; api_style: string | null; base_url: string; api_key_ref?: string; auth_header_name: string; auth_header_prefix: string; proxy_url: string | null; no_proxy: string | null; default_max_tokens?: number; default_temperature?: number; default_timeout_secs?: number; default_timeout_streaming_secs?: number; default_web_search?: string }
export type RequestKindInput = 'chat' | 'fast_chat' | 'vision' | 'audio_chat' | 'transcription' | 'embedding' | 'image_generation' | 'speech_synthesis';
export type RequestKind = 'chat' | 'fast_chat' | 'vision' | 'audio_chat' | 'transcription' | 'embedding' | 'image_generation' | 'speech_synthesis';
export interface RequestPolicyInput { request?: RequestKindInput; primary?: string }
export interface RequestPolicy { request: RequestKind; primary: string }
export interface SettingsInput { default_shell?: ShellChoiceInput; llm?: LlmConfigInput; hotkey?: HotkeyConfigInput; session?: SessionConfigInput; context_limits?: ContextLimitsConfigInput; memory?: MemoryConfigInput; security?: SecurityConfigInput; media?: MediaConfigInput; skills?: SkillsConfigInput; skills_exec?: SkillsExecConfigInput; mcp_discovery?: McpDiscoveryConfigInput; mcp_servers?: McpServerConfigInput[]; notification?: NotificationConfigInput; log?: LogConfigInput; tool_settings?: Record<string, ToolConfigInput> }
export interface Settings { default_shell: ShellChoice; llm: LlmConfig; hotkey: HotkeyConfig; session: SessionConfig; context_limits: ContextLimitsConfig; memory: MemoryConfig; security: SecurityConfig; media: MediaConfig; skills: SkillsConfig; skills_exec: SkillsExecConfig; mcp_discovery: McpDiscoveryConfig; mcp_servers: McpServerConfig[]; notification: NotificationConfig; log: LogConfig; tool_settings: Record<string, ToolConfig> }
export interface AudioConfigInput { sample_rate?: number; channels?: number; bits_per_sample?: number; max_duration_secs?: number; silence_timeout_ms?: number; vad_threshold?: number }
export interface AudioConfig { sample_rate: number; channels: number; bits_per_sample: number; max_duration_secs: number; silence_timeout_ms: number; vad_threshold: number }
export interface ImageGenConfigInput { provider?: string; model?: string; timeout_secs?: number }
export interface ImageGenConfig { provider: string; model: string; timeout_secs: number }
export interface MediaConfigInput { input_strategy?: MediaInputStrategyInput; audio?: AudioConfigInput; stt?: SttConfigInput; ocr?: OcrConfigInput; tts?: TtsConfigInput; image_gen?: ImageGenConfigInput }
export interface MediaConfig { input_strategy: MediaInputStrategy; audio: AudioConfig; stt: SttConfig; ocr: OcrConfig; tts: TtsConfig; image_gen: ImageGenConfig }
export interface OcrConfigInput { provider?: string; api_key?: string; api_key_ref?: string | null; api_secret?: string; api_secret_ref?: string | null; base_url?: string; timeout_secs?: number; min_confidence?: number }
export interface OcrConfig { provider: string; api_key_ref?: string; api_secret_ref?: string; base_url: string; timeout_secs: number; min_confidence: number }
export interface SttConfigInput { provider?: string; mcp_server?: string | null; model?: string; timeout_secs?: number; min_confidence?: number }
export interface SttConfig { provider: string; mcp_server: string | null; model: string; timeout_secs: number; min_confidence: number }
export interface TtsConfigInput { provider?: string; model?: string; voice?: string; timeout_secs?: number }
export interface TtsConfig { provider: string; model: string; voice: string; timeout_secs: number }
export interface ContextLimitsConfigInput { compaction_ratio?: number; compaction_reserve_tokens?: number; default_context_window?: number; max_response_tokens?: number; max_observation_chars?: number; max_transcript_chars?: number; max_attachment_images?: number; max_attachment_files?: number; max_attachment_image_bytes?: number; max_attachment_file_bytes?: number; max_upload_total_bytes?: number; max_attachment_image_dim_px?: number; attachment_image_jpeg_quality?: number; file_read_max_chars?: number; file_line_span?: number; file_max_line_chars?: number; file_summary_input_chars?: number; file_max_list_entries?: number; file_max_byte_read?: number; file_vision_max_bytes?: number; search_snippet_chars?: number; search_max_results?: number; search_max_file_size_bytes?: number; search_window_bytes?: number; notification_summary_chars?: number; action_result_context_chars?: number; partial_checkpoint_min_chars?: number; partial_checkpoint_interval_secs?: number; fact_infer_interval_steps?: number; fact_extraction_min_interval_secs?: number; max_known_facts?: number; sanitize_field_max_chars?: number; file_summary_timeout_secs?: number; turn_deadline_secs?: number; cut_off_retries?: number; empty_response_max_retries?: number; empty_response_retry_delay_ms?: number; stream_stall_warn_delay_ms?: number; reasoning_echo_max_chars?: number; background_job_tail_max_chars?: number; background_job_output_emit_interval_ms?: number; terminal_job_ttl_secs?: number; mcp_max_binary_payload_bytes?: number; mcp_max_sse_buffer_bytes?: number; skills_max_md_bytes?: number; skills_max_parse_lines?: number; skills_max_line_len?: number; self_tool_max_instructions_bytes?: number; self_tool_max_script_bytes?: number; network_max_retries?: number; network_backoff_base_secs?: number; network_max_body_bytes?: number; clipboard_history_entries?: number; clipboard_history_max_entries?: number; clipboard_entry_max_chars?: number; scheduled_actions_max?: number; scheduled_actions_due_horizon_secs?: number; background_max_actions?: number; event_chunk_batch_max_bytes?: number; input_ring_buffer_secs?: number; embedding_chunk_size?: number; max_tools_per_request?: number }
export interface ContextLimitsConfig { compaction_ratio: number; compaction_reserve_tokens: number; default_context_window: number; max_response_tokens: number; max_observation_chars: number; max_transcript_chars: number; max_attachment_images: number; max_attachment_files: number; max_attachment_image_bytes: number; max_attachment_file_bytes: number; max_upload_total_bytes: number; max_attachment_image_dim_px: number; attachment_image_jpeg_quality: number; file_read_max_chars: number; file_line_span: number; file_max_line_chars: number; file_summary_input_chars: number; file_max_list_entries: number; file_max_byte_read: number; file_vision_max_bytes: number; search_snippet_chars: number; search_max_results: number; search_max_file_size_bytes: number; search_window_bytes: number; notification_summary_chars: number; action_result_context_chars: number; partial_checkpoint_min_chars: number; partial_checkpoint_interval_secs: number; fact_infer_interval_steps: number; fact_extraction_min_interval_secs: number; max_known_facts: number; sanitize_field_max_chars: number; file_summary_timeout_secs: number; turn_deadline_secs: number; cut_off_retries: number; empty_response_max_retries: number; empty_response_retry_delay_ms: number; stream_stall_warn_delay_ms: number; reasoning_echo_max_chars: number; background_job_tail_max_chars: number; background_job_output_emit_interval_ms: number; terminal_job_ttl_secs: number; mcp_max_binary_payload_bytes: number; mcp_max_sse_buffer_bytes: number; skills_max_md_bytes: number; skills_max_parse_lines: number; skills_max_line_len: number; self_tool_max_instructions_bytes: number; self_tool_max_script_bytes: number; network_max_retries: number; network_backoff_base_secs: number; network_max_body_bytes: number; clipboard_history_entries: number; clipboard_history_max_entries: number; clipboard_entry_max_chars: number; scheduled_actions_max: number; scheduled_actions_due_horizon_secs: number; background_max_actions: number; event_chunk_batch_max_bytes: number; input_ring_buffer_secs: number; embedding_chunk_size: number; max_tools_per_request: number }
export interface HotkeyConfigInput { mode?: HotkeyModeInput; key_binding?: string; mute_hotkey?: string | null }
export interface HotkeyConfig { mode: HotkeyMode; key_binding: string; mute_hotkey: string | null }
export interface LogConfigInput { level?: LogLevelInput; file_enabled?: boolean; file_path?: string | null }
export interface LogConfig { level: LogLevel; file_enabled: boolean; file_path: string | null }
export type LogLevelInput = 'trace' | 'debug' | 'info' | 'warn' | 'error';
export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error';
export interface McpDiscoveryConfigInput { health_interval_secs?: number; reconnect_initial_ms?: number; reconnect_max_ms?: number; reconnect_max_retries?: number }
export interface McpDiscoveryConfig { health_interval_secs: number; reconnect_initial_ms: number; reconnect_max_ms: number; reconnect_max_retries: number }
export interface McpEnvironmentCredentialRefInput { name: string; credential_ref?: string | null; has_value?: boolean }
export interface McpEnvironmentCredentialRef { name: string; credential_ref?: string; has_value: boolean }
export interface McpServerConfigInput { name?: string; transport?: McpTransportTypeInput; command?: string; args?: string[]; env?: string[]; env_refs?: McpEnvironmentCredentialRefInput[]; cwd?: string | null; url?: string; enabled?: boolean }
export interface McpServerConfig { name: string; transport: McpTransportType; command: string; args: string[]; env_refs?: McpEnvironmentCredentialRef[]; cwd: string | null; url: string; enabled: boolean }
export interface MemoryConfigInput { session_window_size?: number; fact_inference_enabled?: boolean }
export interface MemoryConfig { session_window_size: number; fact_inference_enabled: boolean }
export interface NotificationConfigInput { session_created?: NotifyChannelsInput; session_completed?: NotifyChannelsInput; session_paused?: NotifyChannelsInput; session_resumed?: NotifyChannelsInput; session_error?: NotifyChannelsInput; permission_requested?: NotifyChannelsInput; action_completed?: NotifyChannelsInput }
export interface NotificationConfig { session_created: NotifyChannels; session_completed: NotifyChannels; session_paused: NotifyChannels; session_resumed: NotifyChannels; session_error: NotifyChannels; permission_requested: NotifyChannels; action_completed: NotifyChannels }
export interface NotifyChannelsInput { in_app?: boolean; windows?: boolean }
export interface NotifyChannels { in_app: boolean; windows: boolean }
export interface SecurityConfigInput { permission_mode?: PermissionModeInput; sandbox_mode?: SandboxModeInput; writable_roots?: string[]; network_policy?: NetworkPolicyInput; encrypt_sensitive?: boolean; permissions?: StoredPermissionInput[] }
export interface SecurityConfig { permission_mode: PermissionMode; sandbox_mode: SandboxMode; writable_roots?: string[]; network_policy: NetworkPolicy; encrypt_sensitive: boolean; permissions?: StoredPermission[] }
export interface SessionConfigInput { max_concurrent?: number; history_retention_days?: number; max_steps?: number; session_max_steps?: number | null }
export interface SessionConfig { max_concurrent: number; history_retention_days: number; max_steps: number; session_max_steps?: number }
export interface SkillsConfigInput { root?: string | null; enabled?: string[] | null }
export interface SkillsConfig { root: string | null; enabled?: string[] }
export interface SkillsExecConfigInput { venv_root?: string; work_dir?: string; timeout_secs?: number; max_output_lines?: number; cpu_time_secs?: number | null; max_memory_mb?: number | null }
export interface SkillsExecConfig { venv_root: string; work_dir: string; timeout_secs: number; max_output_lines: number; cpu_time_secs: number | null; max_memory_mb: number | null }
export interface StoredPermissionInput { key: string; effect: PermissionEffectInput }
export interface StoredPermission { key: string; effect: PermissionEffect }
export interface ToolConfigInput { enabled?: boolean; timeout_secs?: number | null; max_output_chars?: number | null; max_retries?: number | null; retry_backoff_secs?: number | null; allowed_paths?: string[]; allowed_domains?: string[]; disabled_operations?: string[]; risk_override?: RiskLevelInput | null }
export interface ToolConfig { enabled: boolean; timeout_secs?: number; max_output_chars?: number; max_retries?: number; retry_backoff_secs?: number; allowed_paths: string[]; allowed_domains: string[]; disabled_operations: string[]; risk_override: RiskLevel | null }
export type ActionStatus = 'waiting' | 'running' | 'completed' | 'failed' | 'cancelled';
export type SessionStatus = 'pending' | 'running' | 'paused' | 'completed' | 'error';
export type SessionWaitingReason = 'user_input' | 'user_interrupt' | 'ask' | 'confirmation' | 'scheduled_confirmation' | 'background_task' | 'scheduled_task' | 'step_budget';
export interface MediaAsset { asset_id: string; content_hash: string; media_type: string; size_bytes: number; filename?: string; source: MediaAssetSource; lifecycle: MediaAssetLifecycle; expires_at?: string }
export type MediaAssetLifecycle = 'request' | 'session' | 'managed' | 'external';
export type MediaAssetSource = 'user_attachment' | 'recording' | 'window_capture' | 'generated' | 'tool_output';
export type MediaDerivationInput = 'ocr' | 'stt' | 'document_extract' | 'image_describe' | 'table_extract' | 'thumbnail' | 'tool';
export type MediaDerivation = 'ocr' | 'stt' | 'document_extract' | 'image_describe' | 'table_extract' | 'thumbnail' | 'tool';
export interface MediaInput { asset: MediaAsset; representations: MediaRepresentation[]; preferred_representation?: MediaRepresentationKind }
export type MediaInputStrategyInput = 'auto' | 'raw_preferred' | 'extracted_preferred' | 'text_only_safe';
export type MediaInputStrategy = 'auto' | 'raw_preferred' | 'extracted_preferred' | 'text_only_safe';
export type MediaProvenanceInput = { kind: 'original' } | { kind: 'derived'; operation: MediaDerivationInput; provider?: string | null; source_kind?: MediaRepresentationKindInput | null };
export type MediaProvenance = { kind: 'original' } | { kind: 'derived'; operation: MediaDerivation; provider?: string | null; source_kind?: MediaRepresentationKind | null };
export interface MediaRepresentationInput { representation: MediaRepresentationKindInput; provenance: MediaProvenanceInput; confidence?: number | null; cost?: MediaRepresentationCostInput | null; availability: MediaRepresentationAvailabilityInput; payload: MediaRepresentationPayloadInput }
export interface MediaRepresentation { representation: MediaRepresentationKind; provenance: MediaProvenance; confidence?: number; cost?: MediaRepresentationCost; availability: MediaRepresentationAvailability; payload: MediaRepresentationPayload }
export type MediaRepresentationAvailabilityInput = { state: 'available' } | { state: 'pending' } | { state: 'unavailable'; reason: string };
export type MediaRepresentationAvailability = { state: 'available' } | { state: 'pending' } | { state: 'unavailable'; reason: string };
export interface MediaRepresentationCostInput { estimated_tokens?: number | null; estimated_usd?: number | null }
export interface MediaRepresentationCost { estimated_tokens?: number; estimated_usd?: number }
export type MediaRepresentationKindInput = 'raw_image' | 'raw_audio' | 'raw_video' | 'extracted_text' | 'transcript' | 'ocr_text' | 'image_description' | 'document_pages' | 'table_data' | 'thumbnail' | 'managed_file_ref';
export type MediaRepresentationKind = 'raw_image' | 'raw_audio' | 'raw_video' | 'extracted_text' | 'transcript' | 'ocr_text' | 'image_description' | 'document_pages' | 'table_data' | 'thumbnail' | 'managed_file_ref';
export type MediaRepresentationPayloadInput = { kind: 'inline_data'; value: { media_type: string; data: string } } | { kind: 'text'; value: string } | { kind: 'structured'; value: unknown } | { kind: 'managed_file_ref'; value: { asset_id: string; filename?: string | null } };
export type MediaRepresentationPayload = { kind: 'inline_data'; value: { media_type: string; data: string } } | { kind: 'text'; value: string } | { kind: 'structured'; value: unknown } | { kind: 'managed_file_ref'; value: { asset_id: string; filename: string | null } };
export interface ToolAvailability { enabled: boolean; available: boolean; availability_reason?: string; requires_connection: boolean; requires_permission: boolean }
export type ToolCatalogGroup = 'haven' | 'system' | 'agent' | 'skills' | 'mcp' | 'other';
export interface ToolIdentity { source: ToolSource; catalog_group: ToolCatalogGroup; root: string; operation: string | null; stable_name: string }
export interface ToolManifest { identity: ToolIdentity; model: ToolModel; policy: ToolPolicy; presentation: ToolPresentation; root_presentation: ToolRootPresentation; prompt: ToolPrompt; availability: ToolAvailability }
export interface ToolModel { name: string; description: string; input_schema: unknown }
export interface ToolPolicy { risk_level: RiskLevel; permission_key: string; confirmation: string; idempotency: string; scope: string; concurrency: string; effect: string; data_sensitivity: string; network_access: string }
export interface ToolPresentation { label: string; renderer: string; icon: string; represented_source: ToolSource }
export interface ToolPrompt { when_to_use: string; when_not_to_use: string; key_operations: string[] }
export interface ToolRootPresentation { label: string; description: string; icon: string }
export type ToolSource = 'builtin' | 'skill' | 'mcp';
export type HotkeyModeInput = 'toggle' | 'hold';
export type HotkeyMode = 'toggle' | 'hold';
export type McpTransportTypeInput = 'stdio' | 'http';
export type McpTransportType = 'stdio' | 'http';
export interface MessageAttachmentInput { asset_id?: string | null; media_type: string; data: string; filename?: string | null; path?: string | null; sha256?: string | null; size_bytes?: number | null; expires_at?: string | null; representations?: MediaRepresentationInput[]; preferred_representation?: MediaRepresentationKindInput | null }
export interface MessageAttachment { asset_id?: string; media_type: string; data: string; filename?: string; path?: string; sha256?: string; size_bytes?: number; expires_at?: string; representations?: MediaRepresentation[]; preferred_representation?: MediaRepresentationKind }
export type NetworkPolicyInput = 'deny' | 'ask' | 'restricted' | 'open';
export type NetworkPolicy = 'deny' | 'ask' | 'restricted' | 'open';
export type PermissionEffectInput = 'allow' | 'deny';
export type PermissionEffect = 'allow' | 'deny';
export type PermissionModeInput = 'default' | 'plan' | 'auto_edit' | 'autonomous';
export type PermissionMode = 'default' | 'plan' | 'auto_edit' | 'autonomous';
export type RiskLevelInput = 'safe' | 'low' | 'medium' | 'high' | 'critical';
export type RiskLevel = 'safe' | 'low' | 'medium' | 'high' | 'critical';
export type SandboxModeInput = 'read_only' | 'workspace_write' | 'full_access';
export type SandboxMode = 'read_only' | 'workspace_write' | 'full_access';
export type ShellChoiceInput = 'powershell' | 'cmd' | 'pwsh';
export type ShellChoice = 'powershell' | 'cmd' | 'pwsh';
export interface ModelInfo { id: string; provider: string; name: string; context_window: number; supports_streaming: boolean; supports_tools: boolean; supports_vision: boolean; cost_per_1k_input_tokens?: number; cost_per_1k_output_tokens?: number }
export type LlmConnectionFailureReason = 'network' | 'timeout' | 'authentication' | 'rate_limited' | 'circuit_open' | 'server' | 'request_rejected' | 'invalid_response' | 'configuration' | 'unknown';
export interface LlmConnectionReport { status: LlmConnectionStatus; reason?: LlmConnectionFailureReason; provider: string; model: string }
export type LlmConnectionStatus = 'ready' | 'disconnected' | 'unconfigured';
export type McpClientStatus = 'Disconnected' | 'Connecting' | 'Connected' | { 'Offline': { error: string } };
export interface McpServerSnapshot { name: string; transport: string; command: string; args: string[]; env: string[]; cwd: string | null; url: string; enabled: boolean; status: McpClientStatus; tools: McpToolInfo[]; last_error: string | null; diagnostic: string | null; last_seen_at: number | null }
export interface McpToolInfo { name: string; description: string; input_schema: unknown }
export interface Fact { id: string; subject: string; predicate: string; object: string; source: string; confidence: number; tags: string[]; created_at: string; mention_count: number; last_seen_at: string | null; source_ref: FactSourceRef | null; durability: number }
export interface FactSourceRef { message_id: string; snippet: string }
export interface Message { id: string; session_id: string; role: string; content: string; message_type: string | null; created_at: string; tool_call_id: string | null; attachments: MessageAttachment[]; media_inputs?: MediaInput[]; voice: boolean }
export interface SessionStep { id: string; session_id: string; step_number: number; action_index: number; thought: string | null; action_tool: string | null; action_input: string | null; tool_call_id: string | null; observation: string | null; status: string; is_high_risk: boolean; confirmed: boolean | null; silent: boolean; started_at: string | null; completed_at: string | null; created_at: string }
export interface Session { id: string; input_text: string; title: string | null; status: SessionStatus; created_at: string; updated_at: string }
export interface LlmCallUsage { id: string; session_id: string; step_number: number | null; role: RequestKind; call_kind: string; model: string | null; prompt_tokens: number; completion_tokens: number; total_tokens: number; cached_tokens: number; cache_creation_tokens: number; cache_miss_tokens: number; cache_accounting: string; cache_diagnostics?: unknown; context_tokens: number; context_window: number | null; cost_usd: number; has_cost: boolean; duration_ms: number | null; created_at: string }
export interface SessionUsage { prompt_tokens: number; completion_tokens: number; total_tokens: number; cached_tokens: number; cache_creation_tokens: number; cache_miss_tokens: number; context_tokens: number; context_window: number | null; cost_usd: number; has_cost: boolean }
export interface SkillInfo { name: string; description: string; version: string | null; language: string; enabled: boolean; root: string; has_script: boolean }

export interface TauriCommandMap {
	add_fact: { request: { subject: string; predicate: string; object: string; tags?: string[] | null }; response: Fact };
	add_mcp_server: { request: { config: McpServerConfigInput }; response: void };
	cancel_action: { request: { actionId: string; kind: ActionKindInput }; response: boolean };
	cancel_recording: { request: undefined; response: void };
	check_llm_connection: { request: undefined; response: LlmConnectionReport };
	check_shell_available: { request: { shell: string }; response: ShellAvailability };
	clear_history: { request: undefined; response: number };
	continue_session: { request: { sessionId: string }; response: void };
	count_history: { request: undefined; response: number };
	count_history_search: { request: { query: string }; response: number };
	delete_action: { request: { actionId: string }; response: boolean };
	delete_fact: { request: { factId: string }; response: void };
	delete_session: { request: { sessionId: string }; response: void };
	disable_autostart: { request: undefined; response: void };
	discard_staged_credentials: { request: undefined; response: void };
	discover_all_models: { request: undefined; response: Record<string, ModelInfo[]> };
	discover_models: { request: { baseUrl: string; apiKey: string; provider?: string | null; role?: string | null; authHeaderName?: string | null; authHeaderPrefix?: string | null; skipAuth?: boolean | null; proxyUrl?: string | null; noProxy?: string | null }; response: ModelInfo[] };
	enable_autostart: { request: undefined; response: void };
	end_session: { request: { sessionId: string }; response: void };
	execute_skill: { request: { name: string; params: unknown }; response: SkillExecutionResponse };
	export_history: { request: { startDate?: string | null; endDate?: string | null; status?: string | null }; response: string };
	get_api_key_status: { request: undefined; response: ApiKeyStatus };
	get_bootstrap_status: { request: undefined; response: string };
	get_history: { request: { limit: number; offset: number }; response: Session[] };
	get_last_conversation: { request: undefined; response: SessionResumeResponse | null };
	get_log_info: { request: undefined; response: LogInfo };
	get_performance_metrics: { request: { ui?: UiMetricsSnapshotInput | null }; response: MetricsSnapshot };
	get_recording_state: { request: undefined; response: RecordingState };
	get_session_for_resume: { request: { sessionId: string }; response: SessionResumeResponse };
	get_sessions: { request: undefined; response: SessionListResponse };
	get_settings: { request: undefined; response: Settings };
	get_tools: { request: undefined; response: ToolListResponse };
	interrupt_session: { request: { sessionId: string }; response: void };
	is_autostart_enabled: { request: undefined; response: boolean };
	list_action_history: { request: { kind?: ActionKindInput | null; limit?: number | null; sessionId?: string | null }; response: ActionEvent[] };
	list_actions: { request: undefined; response: ActionEvent[] };
	list_facts: { request: { source?: string | null }; response: Fact[] };
	list_mcp_tools: { request: undefined; response: McpServerSnapshot[] };
	list_permissions: { request: undefined; response: StoredPermission[] };
	list_session_permissions: { request: undefined; response: SessionPermissionGrant[] };
	list_skills: { request: undefined; response: SkillInfo[] };
	log_frontend_error: { request: { message: string }; response: void };
	mcp_tool_call: { request: { client: string; tool: string; args: unknown }; response: McpToolCallResponse };
	open_external: { request: { target: string }; response: void };
	open_skills_dir: { request: undefined; response: string };
	process_transcript: { request: { transcript: string; activeSessionId?: string | null; attachments?: MessageAttachmentInput[] | null; voice?: boolean | null; recordingSessionId?: string | null }; response: ProcessResult };
	read_log_tail: { request: { maxLines?: number | null }; response: LogTail };
	recall_memory: { request: { query: string; kind?: string | null; limit?: number | null }; response: MemoryRecallItem[] };
	reconnect_mcp: { request: { name: string }; response: void };
	refresh_mcp_servers: { request: undefined; response: McpRefreshResult };
	refresh_skills: { request: undefined; response: void };
	remove_mcp_server: { request: { name: string }; response: void };
	reopen_session: { request: { sessionId: string }; response: void };
	reset_permissions: { request: undefined; response: void };
	reset_session_permissions: { request: undefined; response: number };
	reset_tool_circuits: { request: undefined; response: void };
	resolve_confirmation: { request: { stepId: string; effect: string; scope: string; target: string; timedOut: boolean }; response: void };
	revoke_permission: { request: { key: string }; response: void };
	revoke_session_permission: { request: { sessionId: string; capability: string }; response: void };
	rollback_session: { request: { sessionId: string; targetStep: number; pause?: boolean | null; targetMessageId?: string | null }; response: void };
	run_memory_maintenance: { request: undefined; response: number };
	search_history: { request: { query: string }; response: Session[] };
	search_history_filtered: { request: { query?: string | null; status?: string | null; startDate?: string | null; endDate?: string | null; limit?: number | null; offset?: number | null }; response: Session[] };
	search_history_paginated: { request: { query: string; limit: number; offset: number }; response: Session[] };
	set_hotkey_capture_active: { request: { active: boolean }; response: void };
	set_reasoning_effort: { request: { role: string; effort?: string | null }; response: void };
	set_skill_enabled: { request: { name: string; enabled: boolean }; response: void };
	set_tool_enabled: { request: { name: string; enabled: boolean }; response: void };
	set_web_search: { request: { role: string; mode?: string | null }; response: void };
	stage_ocr_credential: { request: { apiSecret: boolean; value: string }; response: string };
	stage_provider_credential: { request: { providerName: string; apiKey: string }; response: string };
	start_recording: { request: undefined; response: void };
	stop_recording: { request: undefined; response: string };
	switch_model: { request: { role: string; modelId: string }; response: void };
	toggle_mcp_server: { request: { name: string; enabled: boolean }; response: void };
	update_mcp_server: { request: { name: string; config: McpServerConfigInput }; response: void };
	update_session_title: { request: { sessionId: string; title: string }; response: void };
	update_settings: { request: { settings: SettingsInput }; response: void };
}

export type TauriCommandName = keyof TauriCommandMap;
export type TauriCommandRequest<K extends TauriCommandName> = TauriCommandMap[K]['request'];
export type TauriCommandResponse<K extends TauriCommandName> = TauriCommandMap[K]['response'];
export type TauriCommandRequestArgs<K extends TauriCommandName> = TauriCommandRequest<K> extends undefined
	? [request?: undefined]
	: {} extends TauriCommandRequest<K>
		? [request?: TauriCommandRequest<K>]
		: [request: TauriCommandRequest<K>];

export type TauriCommandInvoke = {
<K extends TauriCommandName>(
command: K,
...args: TauriCommandRequestArgs<K>
): Promise<TauriCommandResponse<K>>;
};
