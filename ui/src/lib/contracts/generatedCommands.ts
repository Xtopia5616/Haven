// Generated from Tauri commands and `crates/app-binary/src/events.rs` by `scripts/generate-ipc-contracts.ps1`.
// Do not edit by hand; `scripts/check-ipc-contracts.ps1` rejects drift.

// DTO declarations below are generated from Rust Serialize types.

export const AGENT_EVENT_NAMES = [
	'agent:thought',
	'agent:tool_call',
	'agent:observation',
	'agent:thought_chunk',
	'agent:reasoning_chunk',
	'agent:tool_call_chunk',
	'agent:stream_reset',
	'agent:media_plan',
	'agent:web_search',
	'agent:stream_stalled',
	'agent:supplement',
	'agent:compaction',
	'agent:usage',
	'agent:tool_output',
	'notification:show',
] as const;

export const APP_EVENT_NAMES = [
	'app:bootstrap',
	'tray:status_changed',
	'mute:changed',
	'mcp:status_change',
	'skills:status_change',
	'interaction:requested',
	'hotkey:conflict',
	'hotkey:rebind',
	'llm:config_changed',
] as const;

export const RECORDING_EVENT_NAMES = [
	'recording:started',
	'recording:stopped',
	'recording:vad_status',
	'recording:error',
	'transcription:started',
	'transcription:result',
	'transcription:error',
] as const;

export const SESSION_EVENT_NAMES = [
	'session:lifecycle',
] as const;

export const TOOL_RUN_EVENT_NAMES = [
	'tool_run:created',
	'tool_run:updated',
	'tool_run:output',
	'tool_run:finished',
] as const;

export const INTERACTION_KIND_VALUES = ['ask', 'confirm', 'scheduled_confirm'] as const;
export type InteractionKind = (typeof INTERACTION_KIND_VALUES)[number];
export type InteractionOwnerInput = { kind: 'session'; session_id: string } | { kind: 'scheduled_tool_run'; tool_run_id: string } | { kind: 'app_command' };
export type InteractionOwner = { kind: 'session'; session_id: string } | { kind: 'scheduled_tool_run'; tool_run_id: string } | { kind: 'app_command' };
export const INTERACTION_STATUS_VALUES = ['pending', 'resolved', 'expired', 'cancelled'] as const;
export type InteractionStatus = (typeof INTERACTION_STATUS_VALUES)[number];
export interface CounterSnapshot { turn_starts: number; first_tokens: number; stream_chunks: number; chunk_drops: number; checkpoint_pending: number; branch_point_failures: number; event_boundary_check_failures: number; projection_failures: number; inbox_ack_failures: number; tool_run_result_retries: number; tool_run_result_duplicates: number; web_search_drops: number }
export interface GaugeSnapshot { context_queue_items: number }
export interface MetricsSnapshot { phases: PhaseSnapshot[]; counters: CounterSnapshot; gauges: GaugeSnapshot; ui?: UiMetricsSnapshot }
export interface PhaseSnapshot { count: number; total_ms: number; p50_ms: number; p95_ms: number }
export interface UiMetricsSnapshotInput { frames: number; chunks: number; drops: number }
export interface UiMetricsSnapshot { frames: number; chunks: number; drops: number }
export type ProcessResult = { 'SessionCreated': { session_id: string; message_id?: string | null } } | { 'Supplemented': { session_id: string; message_id?: string | null } };
export const BOOTSTRAP_STATUS_VALUES = ['loading', 'ready'] as const;
export type BootstrapStatus = (typeof BOOTSTRAP_STATUS_VALUES)[number];
export interface RuntimeSessionDto { session_id: string; input_text: string; title: string | null; status: SessionStatus; waiting_reason?: SessionWaitingReason }
export interface RuntimeSessionListResponse { sessions: RuntimeSessionDto[] }
export interface BuiltinToolManifestListResponse { tools: ToolManifest[] }
export interface MemoryFactCursorInput { createdAt: string; factId: string }
export interface MemoryFactCursor { createdAt: string; factId: string }
export interface MemoryFactPageResponse { facts: MemoryFactResponse[]; next_cursor: MemoryFactCursor | null }
export interface MemoryFactResponse { fact_id: string; subject: string; predicate: string; object: string; source: MemoryFactSource; confidence: number; tags: string[]; created_at: string; mention_count: number; last_seen_at: string | null; source_ref: MemoryFactSourceRef | null; durability: number }
export const MEMORY_FACT_SOURCE_INPUT_VALUES = ['user', 'inferred'] as const;
export type MemoryFactSourceInput = (typeof MEMORY_FACT_SOURCE_INPUT_VALUES)[number];
export const MEMORY_FACT_SOURCE_VALUES = ['user', 'inferred'] as const;
export type MemoryFactSource = (typeof MEMORY_FACT_SOURCE_VALUES)[number];
export interface MemoryFactSourceRef { message_id: string; snippet: string }
export interface MemoryRecallItem { entity_id: string; text: string; score: number; model: string }
export interface SandboxOutputReviewChangeDto { changeIndex: number; relativePath: string; change: SandboxOutputReviewChangeKindDto; beforeBytes: number | null; afterBytes: number | null; beforeSha256: string | null; afterSha256: string | null; promoted: boolean }
export const SANDBOX_OUTPUT_REVIEW_CHANGE_KIND_DTO_VALUES = ['created', 'modified', 'deleted', 'type_changed'] as const;
export type SandboxOutputReviewChangeKindDto = (typeof SANDBOX_OUTPUT_REVIEW_CHANGE_KIND_DTO_VALUES)[number];
export interface SandboxOutputReviewManifestDto { sessionId: string; sourceStepId: string; expiresAtUnixMs: number; storageChargeBytes: number; changes: SandboxOutputReviewChangeDto[] }
export interface SkillExecutionResponse { success: boolean; output: unknown; error: string | null }
export const FRONTEND_FAILURE_KIND_INPUT_VALUES = ['operation_failure', 'unhandled_ui_error', 'unhandled_rejection'] as const;
export type FrontendFailureKindInput = (typeof FRONTEND_FAILURE_KIND_INPUT_VALUES)[number];
export interface LogInfo { enabled: boolean; level: LogLevel; path: string | null }
export interface LogTail { path: string; content: string }
export interface McpRefreshResult { added: string[]; removed: string[]; updated: string[]; failed: string[] }
export interface ApiKeyStatus { models: Record<string, boolean>; providers: Record<string, boolean>; stt: boolean; ocr: boolean; ocr_secret: boolean }
export const REASONING_EFFORT_SELECTION_INPUT_VALUES = ['low', 'medium', 'high', 'off'] as const;
export type ReasoningEffortSelectionInput = (typeof REASONING_EFFORT_SELECTION_INPUT_VALUES)[number];
export interface RecordingStatus { is_recording: boolean; is_toggle: boolean }
export const CONFIRMATION_RESOLUTION_RESULT_VALUES = ['resolved', 'expired', 'stale'] as const;
export type ConfirmationResolutionResult = (typeof CONFIRMATION_RESOLUTION_RESULT_VALUES)[number];
export interface SessionLineageChildDto { session: SessionRecordDto; parent_session_id: string }
export interface SessionLineageResponse { session_id: string; parent_session_id: string | null; parent: SessionRecordDto | null; children: SessionLineageChildDto[] }
export interface SessionRecordDto { session_id: string; input_text: string; title: string | null; status: SessionStatus; run_end_reason?: string; created_at: string; updated_at: string }
export interface SessionResumeMessageDto { message_id: string; session_id: string; role: CanonicalRole; content: string; message_type: TranscriptMessageKind | null; created_at: string; tool_call_id: string | null; attachments: MessageAttachment[]; media_inputs?: MediaInput[]; voice: boolean }
export interface SessionResumeResponse { session: SessionRecordDto; messages: SessionResumeMessageDto[]; steps: SessionStep[]; usage: SessionUsage | null; llm_usage: LlmUsageRecord[]; interactions: InteractionRequestedEvent[] }
export interface SessionPermissionGrant { session_id: string; session_title: string | null; capability: string; target: PermissionTarget; effect: PermissionEffect }
export interface ShellAvailability { available: boolean }
export interface AgentCompactionEvent { session_id: string; summary: string; tokens_before: number; tokens_after: number; degraded: boolean; episode_id?: string; event_sequence?: number }
export interface AgentMediaPlanEvent { session_id: string; step_number: number; run_id: number; request_kind: RequestKind; strategy: MediaInputStrategy; projections: MediaProjection[]; notices: MediaPlanNotice[]; event_sequence?: number }
export interface AgentNotificationEvent { session_id?: string; title: string; body: string; notification_kind?: AgentNotificationKind; tool_run_kind?: ToolRunKindDto; tool_run_id?: string; tool_run_status?: ToolRunCompletionStatusDto }
export const AGENT_NOTIFICATION_KIND_VALUES = ['tool_run_completion'] as const;
export type AgentNotificationKind = (typeof AGENT_NOTIFICATION_KIND_VALUES)[number];
export interface AgentObservationEvent { session_id: string; observation: string; tool_name: string; step_number: number; run_id: number; silent: boolean; tool_call_id: string | null; tool_index: number; ask_options: string[]; step_id: string; idempotency: OperationIdempotency; operation_scope: ToolOperationScope; renderer: string; result: ToolResultEnvelope; event_sequence?: number }
export interface AgentReasoningChunkEvent { session_id: string; delta: string; step_number: number; run_id: number; message_id: string; seq: number }
export interface AgentStreamResetEvent { session_id: string; step_number: number; run_id: number; thought_message_id: string; reasoning_message_id: string }
export interface AgentStreamStalledEvent { session_id: string }
export interface AgentSupplementEvent { session_id: string; additional_context: string; step_number: number; run_id: number; message_id?: string; supplement_id: string; inject_source?: InjectSource; event_sequence?: number }
export interface AgentThoughtChunkEvent { session_id: string; delta: string; step_number: number; run_id: number; message_id: string; seq: number }
export interface AgentThoughtEvent { session_id: string; thought: string; step_number: number; run_id: number; message_id: string; event_sequence?: number }
export interface AgentToolCallChunkEvent { session_id: string; preview_id: string; tool_name: string; arguments: string; arguments_truncated: boolean; step_number: number; run_id: number; tool_index: number; seq: number }
export interface AgentToolCallEvent { session_id: string; tool_name: string; input: unknown; step_number: number; run_id: number; tool_call_id: string | null; tool_index: number; step_id: string; suppress_streamed_thought: boolean; silent: boolean; event_sequence?: number }
export interface AgentToolOutputEvent { session_id: string; step_id: string; output: string }
export interface AgentUsageEvent { session_id: string; prompt_tokens: number; completion_tokens: number; total_tokens: number; cached_tokens: number; cache_creation_tokens: number; cache_miss_tokens: number; context_tokens: number; cache_exclusive: boolean; cache_accounting: CacheAccounting; cost_usd: number | null; model: string | null; cumulative_prompt_tokens: number; cumulative_completion_tokens: number; cumulative_total_tokens: number; cumulative_cached_tokens: number; cumulative_cache_creation_tokens: number; cumulative_cache_miss_tokens: number; cache_diagnostics?: CacheDiagnostics; cumulative_cost_usd: number | null; context_window: number | null; step_number?: number; duration_ms?: number; request_kind?: RequestKind; call_kind: LlmCallKind; has_cost: boolean }
export interface AgentWebSearchEvent { session_id: string; phase: WebSearchPhase; step_number: number; run_id: number; call_id?: string; call_index?: number; action?: string; result?: unknown }
export interface AppBootstrapEvent { status: BootstrapStatus }
export interface HotkeyConflictEvent { binding: string; error: string }
export interface HotkeyRebindEvent { old_binding: string; new_binding: string }
export interface InteractionRequestedEvent { request_id: string; session_id?: string; owner: InteractionOwner; kind: InteractionKind; status: InteractionStatus; options?: string[]; tool_name?: string; risk_level?: RiskLevel; summary?: string; permission_key?: string; invocation_step_id?: string; tool_index?: number; tool_call_id?: string; created_at: string; expires_at?: string }
export interface McpStatusChangedEvent { name: string; status: McpClientStatus }
export interface MuteChangedEvent { muted: boolean }
export interface RecordingErrorEvent { recording_id: string; error: string }
export interface RecordingEvent { is_recording: boolean; recording_id?: string; reason?: RecordingStopReasonDto; duration_ms?: number }
export const RECORDING_STOP_REASON_DTO_VALUES = ['manual', 'silence', 'max_duration', 'cancel'] as const;
export type RecordingStopReasonDto = (typeof RECORDING_STOP_REASON_DTO_VALUES)[number];
export type SessionLifecycleEvent = { type: 'created'; session_id: string; status: SessionStatus; waiting_reason?: SessionWaitingReason | null; title: string | null } | { type: 'updated'; session_id: string; status: SessionUpdateStatus; waiting_reason?: SessionWaitingReason | null; title: string; reason?: string | null } | { type: 'completed'; session_id: string; title: string; reason: string } | { type: 'error'; session_id: string; title: string; error: string } | { type: 'title_updated'; session_id: string; title: string } | { type: 'deleted'; session_id: string | null };
export const SESSION_UPDATE_STATUS_VALUES = ['pending', 'running', 'paused'] as const;
export type SessionUpdateStatus = (typeof SESSION_UPDATE_STATUS_VALUES)[number];
export interface SkillsStatusChangedEvent { op: SkillsStatusOperation }
export const SKILLS_STATUS_OPERATION_VALUES = ['refresh', 'auto_refresh', 'toggle'] as const;
export type SkillsStatusOperation = (typeof SKILLS_STATUS_OPERATION_VALUES)[number];
export const TOOL_RUN_COMPLETION_STATUS_DTO_VALUES = ['completed', 'failed'] as const;
export type ToolRunCompletionStatusDto = (typeof TOOL_RUN_COMPLETION_STATUS_DTO_VALUES)[number];
export interface ToolRunEvent { tool_run_id: string; kind: ToolRunKindDto; status?: ToolRunStatus; session_id?: string; source_step_id?: string; started_at?: string; finished_at?: string; due_at?: string; title?: string; body?: string; mode?: ScheduleMode; command?: string; output?: string; error?: string; error_reason?: string; exit_code?: number; preview?: string }
export const TOOL_RUN_KIND_DTO_INPUT_VALUES = ['background', 'scheduled'] as const;
export type ToolRunKindDtoInput = (typeof TOOL_RUN_KIND_DTO_INPUT_VALUES)[number];
export const TOOL_RUN_KIND_DTO_VALUES = ['background', 'scheduled'] as const;
export type ToolRunKindDto = (typeof TOOL_RUN_KIND_DTO_VALUES)[number];
export interface TranscriptionErrorEvent { recording_id: string; error: string }
export interface TranscriptionResultEvent { recording_id: string; text: string; duration_ms: number; confidence?: number }
export interface TranscriptionStartedEvent { recording_id: string }
export interface TrayStatusChangedEvent { status: TrayStatusEventValue; tooltip: string }
export const TRAY_STATUS_EVENT_VALUE_VALUES = ['normal', 'recording', 'muted', 'busy'] as const;
export type TrayStatusEventValue = (typeof TRAY_STATUS_EVENT_VALUE_VALUES)[number];
export const VAD_SIGNAL_DTO_VALUES = ['none', 'speech_start', 'speech_end', 'auto_stop'] as const;
export type VadSignalDto = (typeof VAD_SIGNAL_DTO_VALUES)[number];
export const VAD_STATE_DTO_VALUES = ['silent', 'speech', 'silence_after_speech'] as const;
export type VadStateDto = (typeof VAD_STATE_DTO_VALUES)[number];
export interface VadStatusEvent { signal: VadSignalDto; state: VadStateDto }
export const OCR_CREDENTIAL_FIELD_INPUT_VALUES = ['api_key', 'api_secret'] as const;
export type OcrCredentialFieldInput = (typeof OCR_CREDENTIAL_FIELD_INPUT_VALUES)[number];
export const ALIYUN_SEARCH_CONTENT_TYPE_INPUT_VALUES = ['snippet', 'summary', 'mainText'] as const;
export type AliyunSearchContentTypeInput = (typeof ALIYUN_SEARCH_CONTENT_TYPE_INPUT_VALUES)[number];
export const ALIYUN_SEARCH_CONTENT_TYPE_VALUES = ['snippet', 'summary', 'mainText'] as const;
export type AliyunSearchContentType = (typeof ALIYUN_SEARCH_CONTENT_TYPE_VALUES)[number];
export const ALIYUN_SEARCH_TIME_RANGE_INPUT_VALUES = ['day', 'week', 'month', 'year', 'no_limit'] as const;
export type AliyunSearchTimeRangeInput = (typeof ALIYUN_SEARCH_TIME_RANGE_INPUT_VALUES)[number];
export const ALIYUN_SEARCH_TIME_RANGE_VALUES = ['day', 'week', 'month', 'year', 'no_limit'] as const;
export type AliyunSearchTimeRange = (typeof ALIYUN_SEARCH_TIME_RANGE_VALUES)[number];
export const ALIYUN_SEARCH_WAY_INPUT_VALUES = ['lite', 'pro', 'pro-fetch'] as const;
export type AliyunSearchWayInput = (typeof ALIYUN_SEARCH_WAY_INPUT_VALUES)[number];
export const ALIYUN_SEARCH_WAY_VALUES = ['lite', 'pro', 'pro-fetch'] as const;
export type AliyunSearchWay = (typeof ALIYUN_SEARCH_WAY_VALUES)[number];
export const CAPABILITY_INPUT_VALUES = ['chat', 'fast_chat', 'vision', 'audio_input', 'transcription', 'embedding', 'image_generation', 'speech_synthesis'] as const;
export type CapabilityInput = (typeof CAPABILITY_INPUT_VALUES)[number];
export const CAPABILITY_VALUES = ['chat', 'fast_chat', 'vision', 'audio_input', 'transcription', 'embedding', 'image_generation', 'speech_synthesis'] as const;
export type Capability = (typeof CAPABILITY_VALUES)[number];
export type IndependentSearchConfigInput = { provider: 'deepseek_search'; count?: number; need_content?: boolean; pass_original_question?: boolean; localization?: string | null } | { provider: 'perplexity_search'; max_results?: number; search_type?: PerplexitySearchTypeInput; recency_filter?: PerplexitySearchRecencyInput | null; country?: string | null; max_tokens?: number | null; max_tokens_per_page?: number | null; search_language_filter?: string[]; search_domain_filter?: string[]; last_updated_after_filter?: string | null; last_updated_before_filter?: string | null; search_after_date_filter?: string | null; search_before_date_filter?: string | null } | { provider: 'aliyun_ai_search'; endpoint: string; workspace_name?: string; top_k?: number; query_rewrite?: boolean; content_type?: AliyunSearchContentTypeInput; way?: AliyunSearchWayInput; time_range?: AliyunSearchTimeRangeInput; api_key_ref?: string | null } | { provider: 'mistral_web_search'; premium?: boolean } | { provider: 'zhipu_web_search' };
export type IndependentSearchConfig = { provider: 'deepseek_search'; count: number; need_content: boolean; pass_original_question: boolean; localization?: string | null } | { provider: 'perplexity_search'; max_results: number; search_type: PerplexitySearchType; recency_filter: PerplexitySearchRecency | null; country?: string | null; max_tokens?: number | null; max_tokens_per_page?: number | null; search_language_filter: string[]; search_domain_filter: string[]; last_updated_after_filter?: string | null; last_updated_before_filter?: string | null; search_after_date_filter?: string | null; search_before_date_filter?: string | null } | { provider: 'aliyun_ai_search'; endpoint: string; workspace_name: string; top_k: number; query_rewrite: boolean; content_type: AliyunSearchContentType; way: AliyunSearchWay; time_range: AliyunSearchTimeRange; api_key_ref?: string | null } | { provider: 'mistral_web_search'; premium: boolean } | { provider: 'zhipu_web_search' };
export interface LlmConfigInput { providers?: ProviderConfigInput[]; models?: ModelConfigInput[]; request_policies?: RequestPolicyInput[]; max_total_duration_secs?: number; stream_idle_timeout_secs?: number; retry_max_retries?: number; retry_base_secs?: number; retry_factor?: number; retry_max_secs?: number; retry_jitter?: number; max_concurrent_requests?: number }
export interface LlmConfig { providers: ProviderConfig[]; models: ModelConfig[]; request_policies: RequestPolicy[]; max_total_duration_secs: number; stream_idle_timeout_secs: number; retry_max_retries: number; retry_base_secs: number; retry_factor: number; retry_max_secs: number; retry_jitter: number; max_concurrent_requests: number }
export interface ModelConfigInput { id?: string; provider_name?: string; model?: string; capabilities?: CapabilityInput[]; temperature?: number | null; context_window?: number | null; cost_per_1k_input_tokens?: number | null; cost_per_1k_output_tokens?: number | null; cost_per_1k_cache_read_tokens?: number | null; cost_per_1k_cache_write_tokens?: number | null; max_tokens?: number | null; reasoning_effort?: string | null; web_search?: string | null; independent_search?: IndependentSearchConfigInput | null; reasoning_echo_max_chars?: number | null }
export interface ModelConfig { id: string; provider_name: string; model: string; capabilities: Capability[]; temperature?: number; context_window?: number; cost_per_1k_input_tokens?: number; cost_per_1k_output_tokens?: number; cost_per_1k_cache_read_tokens?: number; cost_per_1k_cache_write_tokens?: number; max_tokens?: number; reasoning_effort?: string; web_search?: string; independent_search?: IndependentSearchConfig; reasoning_echo_max_chars?: number }
export const PERPLEXITY_SEARCH_RECENCY_INPUT_VALUES = ['hour', 'day', 'week', 'month', 'year'] as const;
export type PerplexitySearchRecencyInput = (typeof PERPLEXITY_SEARCH_RECENCY_INPUT_VALUES)[number];
export const PERPLEXITY_SEARCH_RECENCY_VALUES = ['hour', 'day', 'week', 'month', 'year'] as const;
export type PerplexitySearchRecency = (typeof PERPLEXITY_SEARCH_RECENCY_VALUES)[number];
export const PERPLEXITY_SEARCH_TYPE_INPUT_VALUES = ['web', 'fast', 'people'] as const;
export type PerplexitySearchTypeInput = (typeof PERPLEXITY_SEARCH_TYPE_INPUT_VALUES)[number];
export const PERPLEXITY_SEARCH_TYPE_VALUES = ['web', 'fast', 'people'] as const;
export type PerplexitySearchType = (typeof PERPLEXITY_SEARCH_TYPE_VALUES)[number];
export interface ProviderConfigInput { name?: string; provider?: string; api_style?: string | null; media_capabilities?: ProviderMediaCapabilitiesInput | null; base_url?: string; api_key?: string; api_key_ref?: string | null; auth_header_name?: string; auth_header_prefix?: string; proxy_url?: string | null; no_proxy?: string | null; default_max_tokens?: number | null; default_temperature?: number | null; default_timeout_secs?: number | null; default_timeout_streaming_secs?: number | null; default_web_search?: string | null }
export interface ProviderConfig { name: string; provider: string; api_style: string | null; media_capabilities?: ProviderMediaCapabilities; base_url: string; api_key_ref?: string; auth_header_name: string; auth_header_prefix: string; proxy_url: string | null; no_proxy: string | null; default_max_tokens?: number; default_temperature?: number; default_timeout_secs?: number; default_timeout_streaming_secs?: number; default_web_search?: string }
export interface ProviderMediaCapabilitiesInput { stt?: boolean; tts?: boolean; image_gen?: boolean }
export interface ProviderMediaCapabilities { stt: boolean; tts: boolean; image_gen: boolean }
export const REQUEST_KIND_INPUT_VALUES = ['chat', 'fast_chat', 'vision', 'audio_chat', 'transcription', 'embedding', 'image_generation', 'speech_synthesis'] as const;
export type RequestKindInput = (typeof REQUEST_KIND_INPUT_VALUES)[number];
export const REQUEST_KIND_VALUES = ['chat', 'fast_chat', 'vision', 'audio_chat', 'transcription', 'embedding', 'image_generation', 'speech_synthesis'] as const;
export type RequestKind = (typeof REQUEST_KIND_VALUES)[number];
export interface RequestPolicyInput { request?: RequestKindInput; primary?: string }
export interface RequestPolicy { request: RequestKind; primary: string }
export interface SettingsInput { default_shell?: ShellChoiceInput; llm?: LlmConfigInput; hotkey?: HotkeyConfigInput; session?: SessionConfigInput; context_limits?: ContextLimitsConfigInput; memory?: MemoryConfigInput; security?: SecurityConfigInput; media?: MediaConfigInput; skills?: SkillsConfigInput; skills_exec?: SkillsExecConfigInput; mcp_discovery?: McpDiscoveryConfigInput; mcp_servers?: McpServerConfigInput[]; notification?: NotificationConfigInput; log?: LogConfigInput; tool_settings?: Record<string, ToolConfigInput> }
export interface Settings { default_shell: ShellChoice; llm: LlmConfig; hotkey: HotkeyConfig; session: SessionConfig; context_limits: ContextLimitsConfig; memory: MemoryConfig; security: SecurityConfig; media: MediaConfig; skills: SkillsConfig; skills_exec: SkillsExecConfig; mcp_discovery: McpDiscoveryConfig; mcp_servers: McpServerConfig[]; notification: NotificationConfig; log: LogConfig; tool_settings: Record<string, ToolConfig> }
export interface AudioConfigInput { max_duration_secs?: number; silence_timeout_ms?: number; vad_threshold?: number }
export interface AudioConfig { max_duration_secs: number; silence_timeout_ms: number; vad_threshold: number }
export interface ImageGenConfigInput { provider_name?: string; model?: string; timeout_secs?: number }
export interface ImageGenConfig { provider_name: string; model: string; timeout_secs: number }
export interface MediaConfigInput { input_strategy?: MediaInputStrategyInput; audio?: AudioConfigInput; stt?: SttConfigInput; ocr?: OcrConfigInput; tts?: TtsConfigInput; image_gen?: ImageGenConfigInput }
export interface MediaConfig { input_strategy: MediaInputStrategy; audio: AudioConfig; stt: SttConfig; ocr: OcrConfig; tts: TtsConfig; image_gen: ImageGenConfig }
export interface OcrConfigInput { provider?: string; api_key?: string; api_key_ref?: string | null; api_secret?: string; api_secret_ref?: string | null; base_url?: string; timeout_secs?: number; min_confidence?: number }
export interface OcrConfig { provider: string; api_key_ref?: string; api_secret_ref?: string; base_url: string; timeout_secs: number; min_confidence: number }
export interface SttConfigInput { provider_name?: string; mcp_server?: string | null; model?: string; timeout_secs?: number; min_confidence?: number }
export interface SttConfig { provider_name: string; mcp_server: string | null; model: string; timeout_secs: number; min_confidence: number }
export interface TtsConfigInput { provider_name?: string; model?: string; voice?: string; timeout_secs?: number }
export interface TtsConfig { provider_name: string; model: string; voice: string; timeout_secs: number }
export interface ContextLimitsConfigInput { compaction_ratio?: number; compaction_reserve_tokens?: number; default_context_window?: number; response_token_floor?: number; max_observation_chars?: number; max_transcript_chars?: number; max_attachment_images?: number; max_attachment_files?: number; max_attachment_image_bytes?: number; max_attachment_file_bytes?: number; max_upload_total_bytes?: number; max_attachment_image_dim_px?: number; attachment_image_jpeg_quality?: number; file_max_full_file_bytes?: number; file_line_span?: number; file_line_max_bytes?: number; file_summary_input_chars?: number; file_max_list_entries?: number; file_byte_read_max_bytes?: number; file_write_max_bytes?: number; file_vision_max_bytes?: number; search_snippet_chars?: number; search_max_results?: number; search_max_file_size_bytes?: number; search_window_bytes?: number; notification_summary_chars?: number; tool_run_result_context_chars?: number; partial_checkpoint_min_chars?: number; partial_checkpoint_interval_secs?: number; fact_infer_interval_steps?: number; fact_extraction_min_interval_secs?: number; max_known_facts?: number; sanitize_field_max_chars?: number; file_summary_timeout_secs?: number; turn_deadline_secs?: number; incomplete_tool_args_retries?: number; stream_stall_warn_delay_ms?: number; reasoning_echo_max_chars?: number; background_tool_run_tail_max_chars?: number; background_tool_run_output_emit_interval_ms?: number; tool_run_terminal_ttl_secs?: number; mcp_max_binary_base64_bytes?: number; mcp_max_sse_buffer_bytes?: number; skills_max_md_bytes?: number; skills_max_parse_lines?: number; skills_max_line_bytes?: number; self_tool_max_instructions_bytes?: number; self_tool_max_script_bytes?: number; network_max_retries?: number; network_backoff_base_secs?: number; network_max_body_bytes?: number; clipboard_history_entries?: number; clipboard_history_max_entries?: number; clipboard_entry_max_chars?: number; max_pending_scheduled_tool_runs?: number; scheduled_tool_runs_due_horizon_secs?: number; max_running_background_tool_runs?: number; event_chunk_batch_max_bytes?: number; input_ring_buffer_secs?: number; embedding_chunk_size?: number; max_tools_per_request?: number }
export interface ContextLimitsConfig { compaction_ratio: number; compaction_reserve_tokens: number; default_context_window: number; response_token_floor: number; max_observation_chars: number; max_transcript_chars: number; max_attachment_images: number; max_attachment_files: number; max_attachment_image_bytes: number; max_attachment_file_bytes: number; max_upload_total_bytes: number; max_attachment_image_dim_px: number; attachment_image_jpeg_quality: number; file_max_full_file_bytes: number; file_line_span: number; file_line_max_bytes: number; file_summary_input_chars: number; file_max_list_entries: number; file_byte_read_max_bytes: number; file_write_max_bytes: number; file_vision_max_bytes: number; search_snippet_chars: number; search_max_results: number; search_max_file_size_bytes: number; search_window_bytes: number; notification_summary_chars: number; tool_run_result_context_chars: number; partial_checkpoint_min_chars: number; partial_checkpoint_interval_secs: number; fact_infer_interval_steps: number; fact_extraction_min_interval_secs: number; max_known_facts: number; sanitize_field_max_chars: number; file_summary_timeout_secs: number; turn_deadline_secs: number; incomplete_tool_args_retries: number; stream_stall_warn_delay_ms: number; reasoning_echo_max_chars: number; background_tool_run_tail_max_chars: number; background_tool_run_output_emit_interval_ms: number; tool_run_terminal_ttl_secs: number; mcp_max_binary_base64_bytes: number; mcp_max_sse_buffer_bytes: number; skills_max_md_bytes: number; skills_max_parse_lines: number; skills_max_line_bytes: number; self_tool_max_instructions_bytes: number; self_tool_max_script_bytes: number; network_max_retries: number; network_backoff_base_secs: number; network_max_body_bytes: number; clipboard_history_entries: number; clipboard_history_max_entries: number; clipboard_entry_max_chars: number; max_pending_scheduled_tool_runs: number; scheduled_tool_runs_due_horizon_secs: number; max_running_background_tool_runs: number; event_chunk_batch_max_bytes: number; input_ring_buffer_secs: number; embedding_chunk_size: number; max_tools_per_request: number }
export interface HotkeyConfigInput { mode?: HotkeyModeInput; key_binding?: string; mute_hotkey?: string | null }
export interface HotkeyConfig { mode: HotkeyMode; key_binding: string; mute_hotkey: string | null }
export interface LogConfigInput { level?: LogLevelInput; file_enabled?: boolean; file_path?: string | null }
export interface LogConfig { level: LogLevel; file_enabled: boolean; file_path: string | null }
export const LOG_LEVEL_INPUT_VALUES = ['trace', 'debug', 'info', 'warn', 'error'] as const;
export type LogLevelInput = (typeof LOG_LEVEL_INPUT_VALUES)[number];
export const LOG_LEVEL_VALUES = ['trace', 'debug', 'info', 'warn', 'error'] as const;
export type LogLevel = (typeof LOG_LEVEL_VALUES)[number];
export interface McpDiscoveryConfigInput { health_interval_secs?: number; reconnect_initial_ms?: number; reconnect_max_ms?: number; reconnect_max_retries?: number }
export interface McpDiscoveryConfig { health_interval_secs: number; reconnect_initial_ms: number; reconnect_max_ms: number; reconnect_max_retries: number }
export interface McpEnvironmentCredentialRefInput { name: string; credential_ref?: string | null; has_value?: boolean }
export interface McpEnvironmentCredentialRef { name: string; credential_ref?: string; has_value: boolean }
export interface McpExposureConfigInput { default?: McpToolExposureInput; tools?: Record<string, McpToolExposureInput> }
export interface McpExposureConfig { default: McpToolExposure; tools: Record<string, McpToolExposure> }
export interface McpServerConfigInput { name?: string; transport?: McpServerTransportInput; stdio_execution?: McpStdioExecutionInput; command?: string; args?: string[]; env?: string[]; env_refs?: McpEnvironmentCredentialRefInput[]; cwd?: string | null; url?: string; enabled?: boolean; exposure?: McpExposureConfigInput }
export interface McpServerConfig { name: string; transport: McpServerTransport; stdio_execution: McpStdioExecution; command: string; args: string[]; env_refs?: McpEnvironmentCredentialRef[]; cwd: string | null; url: string; enabled: boolean; exposure: McpExposureConfig }
export type McpStdioExecutionInput = { mode: 'host_process' } | { mode: 'managed_package'; package_id: string };
export type McpStdioExecution = { mode: 'host_process' } | { mode: 'managed_package'; package_id: string };
export interface MemoryConfigInput { fact_inference_enabled?: boolean }
export interface MemoryConfig { fact_inference_enabled: boolean }
export interface NotificationConfigInput { session_created?: NotifyChannelsInput; session_completed?: NotifyChannelsInput; session_paused?: NotifyChannelsInput; session_resumed?: NotifyChannelsInput; session_error?: NotifyChannelsInput; permission_requested?: NotifyChannelsInput; tool_run_completed?: NotifyChannelsInput }
export interface NotificationConfig { session_created: NotifyChannels; session_completed: NotifyChannels; session_paused: NotifyChannels; session_resumed: NotifyChannels; session_error: NotifyChannels; permission_requested: NotifyChannels; tool_run_completed: NotifyChannels }
export interface NotifyChannelsInput { in_app?: boolean; windows?: boolean }
export interface NotifyChannels { in_app: boolean; windows: boolean }
export interface SecurityConfigInput { permission_mode?: PermissionModeInput; sandbox_mode?: SandboxModeInput; execution_root?: string | null; writable_roots?: string[]; network_policy?: NetworkPolicyInput; encrypt_sensitive?: boolean; permissions?: StoredPermissionInput[] }
export interface SecurityConfig { permission_mode: PermissionMode; sandbox_mode: SandboxMode; execution_root?: string; writable_roots?: string[]; network_policy: NetworkPolicy; encrypt_sensitive: boolean; permissions?: StoredPermission[] }
export interface SessionConfigInput { max_concurrent?: number; prompt_history_limit?: number; history_retention_days?: number; max_steps_per_run?: number; max_steps_per_session?: number | null }
export interface SessionConfig { max_concurrent: number; prompt_history_limit: number; history_retention_days: number; max_steps_per_run: number; max_steps_per_session?: number }
export interface SkillsConfigInput { root?: string | null; enabled?: string[] | null }
export interface SkillsConfig { root: string | null; enabled?: string[] }
export interface SkillsExecConfigInput { venv_root?: string; work_dir?: string; timeout_secs?: number; max_output_lines?: number }
export interface SkillsExecConfig { venv_root: string; work_dir: string; timeout_secs: number; max_output_lines: number }
export interface StoredPermissionInput { key: string; effect: PermissionEffectInput }
export interface StoredPermission { key: string; effect: PermissionEffect }
export interface ToolConfigInput { enabled?: boolean; timeout_secs?: number | null; max_output_chars?: number | null; max_retries?: number | null; retry_backoff_secs?: number | null; allowed_paths?: string[]; allowed_domains?: string[]; disabled_operations?: string[]; risk_override?: RiskLevelInput | null }
export interface ToolConfig { enabled: boolean; timeout_secs?: number; max_output_chars?: number; max_retries?: number; retry_backoff_secs?: number; allowed_paths: string[]; allowed_domains: string[]; disabled_operations: string[]; risk_override: RiskLevel | null }
export const SESSION_STATUS_VALUES = ['pending', 'running', 'paused', 'completed', 'error'] as const;
export type SessionStatus = (typeof SESSION_STATUS_VALUES)[number];
export const SESSION_STEP_STATUS_VALUES = ['pending', 'running', 'completed', 'failed', 'cancelled', 'unknown'] as const;
export type SessionStepStatus = (typeof SESSION_STEP_STATUS_VALUES)[number];
export const SESSION_WAITING_REASON_VALUES = ['user_input', 'user_interrupt', 'ask', 'confirmation', 'scheduled_confirmation', 'background_task', 'scheduled_task', 'step_budget', 'end_incomplete'] as const;
export type SessionWaitingReason = (typeof SESSION_WAITING_REASON_VALUES)[number];
export const TOOL_RUN_STATUS_VALUES = ['waiting', 'running', 'completed', 'failed', 'cancelled'] as const;
export type ToolRunStatus = (typeof TOOL_RUN_STATUS_VALUES)[number];
export const MCP_SERVER_TRANSPORT_INPUT_VALUES = ['stdio', 'http'] as const;
export type McpServerTransportInput = (typeof MCP_SERVER_TRANSPORT_INPUT_VALUES)[number];
export const MCP_SERVER_TRANSPORT_VALUES = ['stdio', 'http'] as const;
export type McpServerTransport = (typeof MCP_SERVER_TRANSPORT_VALUES)[number];
export const MCP_TOOL_EXPOSURE_INPUT_VALUES = ['direct', 'model_only', 'deferred', 'codemode', 'hidden'] as const;
export type McpToolExposureInput = (typeof MCP_TOOL_EXPOSURE_INPUT_VALUES)[number];
export const MCP_TOOL_EXPOSURE_VALUES = ['direct', 'model_only', 'deferred', 'codemode', 'hidden'] as const;
export type McpToolExposure = (typeof MCP_TOOL_EXPOSURE_VALUES)[number];
export interface MediaAsset { asset_id: string; content_hash: string; media_type: string; size_bytes: number; filename?: string; source: MediaAssetSource; lifecycle: MediaAssetLifecycle; expires_at?: string }
export const MEDIA_ASSET_LIFECYCLE_VALUES = ['request', 'session', 'managed', 'external'] as const;
export type MediaAssetLifecycle = (typeof MEDIA_ASSET_LIFECYCLE_VALUES)[number];
export const MEDIA_ASSET_SOURCE_VALUES = ['user_attachment', 'recording', 'window_capture', 'generated', 'tool_output'] as const;
export type MediaAssetSource = (typeof MEDIA_ASSET_SOURCE_VALUES)[number];
export const MEDIA_DERIVATION_INPUT_VALUES = ['ocr', 'stt', 'document_extract', 'image_describe', 'table_extract', 'thumbnail', 'tool'] as const;
export type MediaDerivationInput = (typeof MEDIA_DERIVATION_INPUT_VALUES)[number];
export const MEDIA_DERIVATION_VALUES = ['ocr', 'stt', 'document_extract', 'image_describe', 'table_extract', 'thumbnail', 'tool'] as const;
export type MediaDerivation = (typeof MEDIA_DERIVATION_VALUES)[number];
export interface MediaInput { asset: MediaAsset; representations: MediaRepresentation[]; preferred_representation?: MediaRepresentationKind }
export const MEDIA_INPUT_STRATEGY_INPUT_VALUES = ['auto', 'raw_preferred', 'extracted_preferred', 'text_only_safe'] as const;
export type MediaInputStrategyInput = (typeof MEDIA_INPUT_STRATEGY_INPUT_VALUES)[number];
export const MEDIA_INPUT_STRATEGY_VALUES = ['auto', 'raw_preferred', 'extracted_preferred', 'text_only_safe'] as const;
export type MediaInputStrategy = (typeof MEDIA_INPUT_STRATEGY_VALUES)[number];
export interface MediaPlanNotice { asset_id: string; code: MediaPlanNoticeCode }
export const MEDIA_PLAN_NOTICE_CODE_VALUES = ['raw_capability_unsupported', 'raw_capability_unknown', 'raw_mime_unsupported', 'raw_size_exceeded', 'derived_fallback', 'managed_reference_fallback', 'strategy_excluded', 'input_part_limit', 'no_compatible_representation'] as const;
export type MediaPlanNoticeCode = (typeof MEDIA_PLAN_NOTICE_CODE_VALUES)[number];
export interface MediaProjection { asset_id: string; representation: MediaRepresentationKind; mode: MediaProjectionMode; provenance: MediaProvenance }
export const MEDIA_PROJECTION_MODE_VALUES = ['raw', 'derived', 'managed_reference'] as const;
export type MediaProjectionMode = (typeof MEDIA_PROJECTION_MODE_VALUES)[number];
export type MediaProvenanceInput = { kind: 'original' } | { kind: 'derived'; operation: MediaDerivationInput; provider?: string | null; source_kind?: MediaRepresentationKindInput | null };
export type MediaProvenance = { kind: 'original' } | { kind: 'derived'; operation: MediaDerivation; provider?: string | null; source_kind?: MediaRepresentationKind | null };
export interface MediaRepresentationInput { representation: MediaRepresentationKindInput; provenance: MediaProvenanceInput; confidence?: number | null; cost?: MediaRepresentationCostInput | null; availability: MediaRepresentationAvailabilityInput; payload: MediaRepresentationPayloadInput }
export interface MediaRepresentation { representation: MediaRepresentationKind; provenance: MediaProvenance; confidence?: number; cost?: MediaRepresentationCost; availability: MediaRepresentationAvailability; payload: MediaRepresentationPayload }
export type MediaRepresentationAvailabilityInput = { state: 'available' } | { state: 'pending' } | { state: 'unavailable'; reason: string };
export type MediaRepresentationAvailability = { state: 'available' } | { state: 'pending' } | { state: 'unavailable'; reason: string };
export interface MediaRepresentationCostInput { estimated_tokens?: number | null; estimated_usd?: number | null }
export interface MediaRepresentationCost { estimated_tokens?: number; estimated_usd?: number }
export const MEDIA_REPRESENTATION_KIND_INPUT_VALUES = ['raw_image', 'raw_audio', 'raw_video', 'extracted_text', 'transcript', 'ocr_text', 'image_description', 'document_pages', 'table_data', 'thumbnail', 'managed_file_ref'] as const;
export type MediaRepresentationKindInput = (typeof MEDIA_REPRESENTATION_KIND_INPUT_VALUES)[number];
export const MEDIA_REPRESENTATION_KIND_VALUES = ['raw_image', 'raw_audio', 'raw_video', 'extracted_text', 'transcript', 'ocr_text', 'image_description', 'document_pages', 'table_data', 'thumbnail', 'managed_file_ref'] as const;
export type MediaRepresentationKind = (typeof MEDIA_REPRESENTATION_KIND_VALUES)[number];
export type MediaRepresentationPayloadInput = { kind: 'inline_data'; value: { media_type: string; data: string } } | { kind: 'text'; value: string } | { kind: 'structured'; value: unknown } | { kind: 'managed_file_ref'; value: { asset_id: string; filename?: string | null } };
export type MediaRepresentationPayload = { kind: 'inline_data'; value: { media_type: string; data: string } } | { kind: 'text'; value: string } | { kind: 'structured'; value: unknown } | { kind: 'managed_file_ref'; value: { asset_id: string; filename: string | null } };
export const SCHEDULE_MODE_VALUES = ['tool', 'continue'] as const;
export type ScheduleMode = (typeof SCHEDULE_MODE_VALUES)[number];
export const CONFIRMATION_REQUIREMENT_VALUES = ['none', 'security_policy', 'required'] as const;
export type ConfirmationRequirement = (typeof CONFIRMATION_REQUIREMENT_VALUES)[number];
export const DATA_SENSITIVITY_VALUES = ['none', 'user_data', 'sensitive'] as const;
export type DataSensitivity = (typeof DATA_SENSITIVITY_VALUES)[number];
export const NETWORK_ACCESS_VALUES = ['none', 'public', 'opaque'] as const;
export type NetworkAccess = (typeof NETWORK_ACCESS_VALUES)[number];
export const OPERATION_EFFECT_VALUES = ['read_only', 'workspace_write', 'external_effect'] as const;
export type OperationEffect = (typeof OPERATION_EFFECT_VALUES)[number];
export const OPERATION_IDEMPOTENCY_VALUES = ['idempotent', 'non_idempotent', 'unknown'] as const;
export type OperationIdempotency = (typeof OPERATION_IDEMPOTENCY_VALUES)[number];
export interface ToolAvailability { enabled: boolean; available: boolean; availability_reason?: string; requires_connection: boolean; requires_permission: boolean }
export const TOOL_CATALOG_GROUP_VALUES = ['haven', 'system', 'agent', 'skills', 'mcp', 'other'] as const;
export type ToolCatalogGroup = (typeof TOOL_CATALOG_GROUP_VALUES)[number];
export const TOOL_CONCURRENCY_MODE_VALUES = ['read_only', 'shared_resource', 'resource', 'exclusive'] as const;
export type ToolConcurrencyMode = (typeof TOOL_CONCURRENCY_MODE_VALUES)[number];
export const TOOL_ERROR_CLASS_VALUES = ['transient', 'unknown_outcome', 'validation', 'permission', 'side_effect_may_have_happened', 'other'] as const;
export type ToolErrorClass = (typeof TOOL_ERROR_CLASS_VALUES)[number];
export const TOOL_EXECUTION_OUTCOME_VALUES = ['succeeded', 'failed', 'cancelled', 'timed_out_and_terminated', 'timed_out_unknown'] as const;
export type ToolExecutionOutcome = (typeof TOOL_EXECUTION_OUTCOME_VALUES)[number];
export interface ToolIdentity { source: ToolSource; catalog_group: ToolCatalogGroup; root: string; operation: string | null; stable_name: string }
export interface ToolManifest { identity: ToolIdentity; model: ToolModel; policy: ToolPolicy; presentation: ToolPresentation; root_presentation: ToolRootPresentation; prompt: ToolPrompt; availability: ToolAvailability }
export interface ToolModel { name: string; description: string; input_schema: unknown }
export const TOOL_OPERATION_SCOPE_VALUES = ['global', 'session'] as const;
export type ToolOperationScope = (typeof TOOL_OPERATION_SCOPE_VALUES)[number];
export interface ToolPolicy { risk_level: RiskLevel; permission_key: string; confirmation: ConfirmationRequirement; idempotency: OperationIdempotency; scope: ToolOperationScope; concurrency: ToolConcurrencyMode; effect: OperationEffect; data_sensitivity: DataSensitivity; network_access: NetworkAccess }
export interface ToolPresentation { label: string; renderer: string; icon: string; represented_source: ToolSource }
export interface ToolPrompt { when_to_use: string; when_not_to_use: string; key_operations: string[] }
export interface ToolResultEnvelope { outcome: ToolExecutionOutcome; error_class?: ToolErrorClass; retry_safety: OperationIdempotency; retryability: ToolRetryability; verification_hint?: string; next_action?: string; assets: string[]; sandbox_output_review_step_ids?: string[] }
export const TOOL_RETRYABILITY_VALUES = ['retryable', 'not_retryable', 'unknown'] as const;
export type ToolRetryability = (typeof TOOL_RETRYABILITY_VALUES)[number];
export interface ToolRootPresentation { label: string; description: string; icon: string }
export const TOOL_SOURCE_VALUES = ['builtin', 'skill', 'mcp'] as const;
export type ToolSource = (typeof TOOL_SOURCE_VALUES)[number];
export const CANONICAL_ROLE_VALUES = ['system', 'user', 'assistant', 'tool'] as const;
export type CanonicalRole = (typeof CANONICAL_ROLE_VALUES)[number];
export const HOTKEY_MODE_INPUT_VALUES = ['toggle', 'hold'] as const;
export type HotkeyModeInput = (typeof HOTKEY_MODE_INPUT_VALUES)[number];
export const HOTKEY_MODE_VALUES = ['toggle', 'hold'] as const;
export type HotkeyMode = (typeof HOTKEY_MODE_VALUES)[number];
export const INJECT_SOURCE_VALUES = ['steering', 'follow_up', 'answer', 'tool_run_result', 'cross_session'] as const;
export type InjectSource = (typeof INJECT_SOURCE_VALUES)[number];
export const INPUT_DELIVERY_MODE_INPUT_VALUES = ['auto', 'steering', 'follow_up'] as const;
export type InputDeliveryModeInput = (typeof INPUT_DELIVERY_MODE_INPUT_VALUES)[number];
export interface MessageAttachmentInput { asset_id?: string | null; media_type: string; data: string; filename?: string | null; path?: string | null; sha256?: string | null; size_bytes?: number | null; expires_at?: string | null; representations?: MediaRepresentationInput[]; preferred_representation?: MediaRepresentationKindInput | null }
export interface MessageAttachment { asset_id?: string; media_type: string; data: string; filename?: string; path?: string; sha256?: string; size_bytes?: number; expires_at?: string; representations?: MediaRepresentation[]; preferred_representation?: MediaRepresentationKind }
export const NETWORK_POLICY_INPUT_VALUES = ['deny', 'ask', 'restricted', 'open'] as const;
export type NetworkPolicyInput = (typeof NETWORK_POLICY_INPUT_VALUES)[number];
export const NETWORK_POLICY_VALUES = ['deny', 'ask', 'restricted', 'open'] as const;
export type NetworkPolicy = (typeof NETWORK_POLICY_VALUES)[number];
export const PERMISSION_EFFECT_INPUT_VALUES = ['allow', 'deny'] as const;
export type PermissionEffectInput = (typeof PERMISSION_EFFECT_INPUT_VALUES)[number];
export const PERMISSION_EFFECT_VALUES = ['allow', 'deny'] as const;
export type PermissionEffect = (typeof PERMISSION_EFFECT_VALUES)[number];
export const PERMISSION_MODE_INPUT_VALUES = ['default', 'plan', 'auto_edit', 'autonomous'] as const;
export type PermissionModeInput = (typeof PERMISSION_MODE_INPUT_VALUES)[number];
export const PERMISSION_MODE_VALUES = ['default', 'plan', 'auto_edit', 'autonomous'] as const;
export type PermissionMode = (typeof PERMISSION_MODE_VALUES)[number];
export const PERMISSION_SCOPE_INPUT_VALUES = ['once', 'session', 'always'] as const;
export type PermissionScopeInput = (typeof PERMISSION_SCOPE_INPUT_VALUES)[number];
export const PERMISSION_TARGET_INPUT_VALUES = ['operation', 'group', 'tool'] as const;
export type PermissionTargetInput = (typeof PERMISSION_TARGET_INPUT_VALUES)[number];
export const PERMISSION_TARGET_VALUES = ['operation', 'group', 'tool'] as const;
export type PermissionTarget = (typeof PERMISSION_TARGET_VALUES)[number];
export const RISK_LEVEL_INPUT_VALUES = ['safe', 'low', 'medium', 'high', 'critical'] as const;
export type RiskLevelInput = (typeof RISK_LEVEL_INPUT_VALUES)[number];
export const RISK_LEVEL_VALUES = ['safe', 'low', 'medium', 'high', 'critical'] as const;
export type RiskLevel = (typeof RISK_LEVEL_VALUES)[number];
export const SANDBOX_MODE_INPUT_VALUES = ['read_only', 'workspace_write', 'full_access'] as const;
export type SandboxModeInput = (typeof SANDBOX_MODE_INPUT_VALUES)[number];
export const SANDBOX_MODE_VALUES = ['read_only', 'workspace_write', 'full_access'] as const;
export type SandboxMode = (typeof SANDBOX_MODE_VALUES)[number];
export const SHELL_CHOICE_INPUT_VALUES = ['powershell', 'cmd', 'pwsh'] as const;
export type ShellChoiceInput = (typeof SHELL_CHOICE_INPUT_VALUES)[number];
export const SHELL_CHOICE_VALUES = ['powershell', 'cmd', 'pwsh'] as const;
export type ShellChoice = (typeof SHELL_CHOICE_VALUES)[number];
export const TRANSCRIPT_MESSAGE_KIND_VALUES = ['text', 'thought', 'tool_call', 'observation', 'reasoning', 'peer_kickoff'] as const;
export type TranscriptMessageKind = (typeof TRANSCRIPT_MESSAGE_KIND_VALUES)[number];
export const CACHE_ACCOUNTING_VALUES = ['inclusive', 'exclusive', 'unknown'] as const;
export type CacheAccounting = (typeof CACHE_ACCOUNTING_VALUES)[number];
export const CACHE_DIAGNOSTIC_OUTCOME_VALUES = ['disabled', 'unknown', 'hit', 'miss'] as const;
export type CacheDiagnosticOutcome = (typeof CACHE_DIAGNOSTIC_OUTCOME_VALUES)[number];
export interface CacheDiagnostics { strategy: PromptCacheStrategy; provider: string; key_requested: boolean; system_split: boolean; downgraded: boolean; outcome: CacheDiagnosticOutcome; usage_source: CacheUsageSource }
export const CACHE_USAGE_SOURCE_VALUES = ['provider', 'unavailable'] as const;
export type CacheUsageSource = (typeof CACHE_USAGE_SOURCE_VALUES)[number];
export const LLM_CALL_KIND_VALUES = ['agent', 'media', 'tool'] as const;
export type LlmCallKind = (typeof LLM_CALL_KIND_VALUES)[number];
export const PROMPT_CACHE_STRATEGY_VALUES = ['off', 'key', 'split', 'implicit', 'explicit'] as const;
export type PromptCacheStrategy = (typeof PROMPT_CACHE_STRATEGY_VALUES)[number];
export const WEB_SEARCH_MODE_INPUT_VALUES = ['off', 'auto', 'always'] as const;
export type WebSearchModeInput = (typeof WEB_SEARCH_MODE_INPUT_VALUES)[number];
export interface ModelInfo { id: string; provider: string; name: string; context_window: number; supports_streaming: boolean; supports_tools: boolean; supports_vision: boolean; cost_per_1k_input_tokens?: number; cost_per_1k_output_tokens?: number }
export const LLM_CONNECTION_FAILURE_REASON_VALUES = ['network', 'timeout', 'authentication', 'rate_limited', 'circuit_open', 'server', 'request_rejected', 'invalid_response', 'configuration', 'unknown'] as const;
export type LlmConnectionFailureReason = (typeof LLM_CONNECTION_FAILURE_REASON_VALUES)[number];
export interface LlmConnectionReport { status: LlmConnectionStatus; reason?: LlmConnectionFailureReason; provider: string; model: string }
export const LLM_CONNECTION_STATUS_VALUES = ['ready', 'disconnected', 'unconfigured'] as const;
export type LlmConnectionStatus = (typeof LLM_CONNECTION_STATUS_VALUES)[number];
export const WEB_SEARCH_PHASE_VALUES = ['in_progress', 'searching', 'completed'] as const;
export type WebSearchPhase = (typeof WEB_SEARCH_PHASE_VALUES)[number];
export const MCP_CACHE_SCOPE_VALUES = ['public', 'private'] as const;
export type McpCacheScope = (typeof MCP_CACHE_SCOPE_VALUES)[number];
export interface McpPrompt { description: string | null; messages: McpPromptMessage[]; ttlMs: number | null; cacheScope: McpCacheScope }
export interface McpPromptArgument { name: string; title?: string; description?: string; required: boolean }
export type McpPromptContent = { type: 'text'; text: string } | { type: 'image'; data: string; mimeType: string } | { type: 'audio'; data: string; mimeType: string } | { type: 'resource_link'; uri: string; name: string; title?: string | null; description?: string | null; mimeType?: string | null; size?: number | null } | { type: 'resource'; resource: McpResourceContent };
export interface McpPromptInfo { name: string; title?: string; description?: string; arguments: McpPromptArgument[] }
export interface McpPromptList { prompts: McpPromptInfo[]; ttlMs: number | null; cacheScope: McpCacheScope }
export interface McpPromptMessage { role: string; content: McpPromptContent }
export interface McpResourceContent { uri: string; mimeType?: string; text?: string; blob?: string }
export interface McpResourceInfo { uri: string; name: string; title?: string; description?: string; mimeType?: string; size?: number }
export interface McpResourceList { resources: McpResourceInfo[]; ttlMs: number | null; cacheScope: McpCacheScope }
export interface McpResourceRead { contents: McpResourceContent[]; ttlMs: number | null; cacheScope: McpCacheScope }
export type McpClientStatus = 'Disconnected' | 'Connecting' | 'Connected' | { 'Offline': { error: string } };
export interface McpServerSnapshot { name: string; transport: McpServerTransport; stdio_execution: McpStdioExecution; command: string; args: string[]; env: string[]; cwd: string | null; url: string; enabled: boolean; target_binding: string | null; supports_resources: boolean; supports_prompts: boolean; exposure: McpExposureConfig; status: McpClientStatus; tools: McpToolInfo[]; last_error: string | null; diagnostic: string | null; last_seen_at: number | null }
export interface McpToolInfo { name: string; description: string; input_schema: unknown }
export const MEMORY_ENTITY_KIND_INPUT_VALUES = ['fact', 'episode'] as const;
export type MemoryEntityKindInput = (typeof MEMORY_ENTITY_KIND_INPUT_VALUES)[number];
export const SESSION_HISTORY_STATUS_FILTER_INPUT_VALUES = ['pending', 'running', 'paused', 'completed', 'error'] as const;
export type SessionHistoryStatusFilterInput = (typeof SESSION_HISTORY_STATUS_FILTER_INPUT_VALUES)[number];
export interface SessionStep { step_id: string; session_id: string; step_number: number; tool_index: number; thought: string | null; tool_name: string | null; tool_input: string | null; tool_call_id: string | null; observation: string | null; status: SessionStepStatus; is_high_risk: boolean; confirmed: boolean | null; silent: boolean; started_at: string | null; completed_at: string | null; created_at: string }
export interface LlmUsageRecord { usage_id: string; session_id: string; step_number: number | null; request_kind: RequestKind; call_kind: LlmCallKind; model: string | null; prompt_tokens: number; completion_tokens: number; total_tokens: number; cached_tokens: number; cache_creation_tokens: number; cache_miss_tokens: number; cache_accounting: CacheAccounting; cache_diagnostics?: CacheDiagnostics; context_tokens: number; context_window: number | null; cost_usd: number; has_cost: boolean; duration_ms: number | null; created_at: string }
export interface SessionUsage { prompt_tokens: number; completion_tokens: number; total_tokens: number; cached_tokens: number; cache_creation_tokens: number; cache_miss_tokens: number; context_tokens: number; context_window: number | null; cost_usd: number; has_cost: boolean }
export interface SkillInfo { name: string; description: string; version: string | null; language: string; enabled: boolean; executable: boolean; has_script: boolean; unavailable_reason: string | null; manifest_error: string | null }
export const MCP_CLIENT_STATUS_UNIT_VALUES = ['Disconnected', 'Connecting', 'Connected'] as const;
export const ADD_FACT_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'fact_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'subject': {
			optional: false,
			shape: { kind: 'string' },
		},
		'predicate': {
			optional: false,
			shape: { kind: 'string' },
		},
		'object': {
			optional: false,
			shape: { kind: 'string' },
		},
		'source': {
			optional: false,
			shape: { kind: 'enum', values: MEMORY_FACT_SOURCE_VALUES },
		},
		'confidence': {
			optional: false,
			shape: { kind: 'number' },
		},
		'tags': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'created_at': {
			optional: false,
			shape: { kind: 'string' },
		},
		'mention_count': {
			optional: false,
			shape: { kind: 'number' },
		},
		'last_seen_at': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'source_ref': {
			optional: false,
			shape: { kind: 'nullable', inner: {
				kind: 'object',
				fields: {
					'message_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'snippet': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'durability': {
			optional: false,
			shape: { kind: 'number' },
		}
	}
} as const;
export const ADD_MCP_SERVER_RESPONSE_SHAPE = { kind: 'null' } as const;
export const AGENT_COMPACTION_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'summary': {
			optional: false,
			shape: { kind: 'string' },
		},
		'tokens_before': {
			optional: false,
			shape: { kind: 'number' },
		},
		'tokens_after': {
			optional: false,
			shape: { kind: 'number' },
		},
		'degraded': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'episode_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'event_sequence': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_MEDIA_PLAN_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'request_kind': {
			optional: false,
			shape: { kind: 'enum', values: REQUEST_KIND_VALUES },
		},
		'strategy': {
			optional: false,
			shape: { kind: 'enum', values: MEDIA_INPUT_STRATEGY_VALUES },
		},
		'projections': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'asset_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'representation': {
						optional: false,
						shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
					},
					'mode': {
						optional: false,
						shape: { kind: 'enum', values: MEDIA_PROJECTION_MODE_VALUES },
					},
					'provenance': {
						optional: false,
						shape: { kind: 'union', anyOf: [
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'original' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'derived' },
									},
									'operation': {
										optional: false,
										shape: { kind: 'enum', values: MEDIA_DERIVATION_VALUES },
									},
									'provider': {
										optional: true,
										shape: { kind: 'string' },
									},
									'source_kind': {
										optional: true,
										shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
									}
								}
							}
						] },
					}
				}
			} },
		},
		'notices': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'asset_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'code': {
						optional: false,
						shape: { kind: 'enum', values: MEDIA_PLAN_NOTICE_CODE_VALUES },
					}
				}
			} },
		},
		'event_sequence': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_OBSERVATION_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'observation': {
			optional: false,
			shape: { kind: 'string' },
		},
		'tool_name': {
			optional: false,
			shape: { kind: 'string' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'silent': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'tool_call_id': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'tool_index': {
			optional: false,
			shape: { kind: 'number' },
		},
		'ask_options': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'step_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'idempotency': {
			optional: false,
			shape: { kind: 'enum', values: OPERATION_IDEMPOTENCY_VALUES },
		},
		'operation_scope': {
			optional: false,
			shape: { kind: 'enum', values: TOOL_OPERATION_SCOPE_VALUES },
		},
		'renderer': {
			optional: false,
			shape: { kind: 'string' },
		},
		'result': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'outcome': {
						optional: false,
						shape: { kind: 'enum', values: TOOL_EXECUTION_OUTCOME_VALUES },
					},
					'error_class': {
						optional: true,
						shape: { kind: 'enum', values: TOOL_ERROR_CLASS_VALUES },
					},
					'retry_safety': {
						optional: false,
						shape: { kind: 'enum', values: OPERATION_IDEMPOTENCY_VALUES },
					},
					'retryability': {
						optional: false,
						shape: { kind: 'enum', values: TOOL_RETRYABILITY_VALUES },
					},
					'verification_hint': {
						optional: true,
						shape: { kind: 'string' },
					},
					'next_action': {
						optional: true,
						shape: { kind: 'string' },
					},
					'assets': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'sandbox_output_review_step_ids': {
						optional: true,
						shape: { kind: 'array', items: { kind: 'string' } },
					}
				}
			},
		},
		'event_sequence': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_REASONING_CHUNK_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'delta': {
			optional: false,
			shape: { kind: 'string' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'message_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'seq': {
			optional: false,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_STREAM_RESET_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'thought_message_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'reasoning_message_id': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const AGENT_STREAM_STALLED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const AGENT_SUPPLEMENT_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'additional_context': {
			optional: false,
			shape: { kind: 'string' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'message_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'supplement_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'inject_source': {
			optional: true,
			shape: { kind: 'enum', values: INJECT_SOURCE_VALUES },
		},
		'event_sequence': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_THOUGHT_CHUNK_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'delta': {
			optional: false,
			shape: { kind: 'string' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'message_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'seq': {
			optional: false,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_THOUGHT_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'thought': {
			optional: false,
			shape: { kind: 'string' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'message_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'event_sequence': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_TOOL_CALL_CHUNK_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'preview_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'tool_name': {
			optional: false,
			shape: { kind: 'string' },
		},
		'arguments': {
			optional: false,
			shape: { kind: 'string' },
		},
		'arguments_truncated': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'tool_index': {
			optional: false,
			shape: { kind: 'number' },
		},
		'seq': {
			optional: false,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_TOOL_CALL_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'tool_name': {
			optional: false,
			shape: { kind: 'string' },
		},
		'input': {
			optional: false,
			shape: { kind: 'unknown' },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'tool_call_id': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'tool_index': {
			optional: false,
			shape: { kind: 'number' },
		},
		'step_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'suppress_streamed_thought': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'silent': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'event_sequence': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const AGENT_TOOL_OUTPUT_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'step_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'output': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const AGENT_USAGE_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'prompt_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'completion_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'total_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cached_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cache_creation_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cache_miss_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'context_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cache_exclusive': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'cache_accounting': {
			optional: false,
			shape: { kind: 'enum', values: CACHE_ACCOUNTING_VALUES },
		},
		'cost_usd': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'number' } },
		},
		'model': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'cumulative_prompt_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cumulative_completion_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cumulative_total_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cumulative_cached_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cumulative_cache_creation_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cumulative_cache_miss_tokens': {
			optional: false,
			shape: { kind: 'number' },
		},
		'cache_diagnostics': {
			optional: true,
			shape: {
				kind: 'object',
				fields: {
					'strategy': {
						optional: false,
						shape: { kind: 'enum', values: PROMPT_CACHE_STRATEGY_VALUES },
					},
					'provider': {
						optional: false,
						shape: { kind: 'string' },
					},
					'key_requested': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'system_split': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'downgraded': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'outcome': {
						optional: false,
						shape: { kind: 'enum', values: CACHE_DIAGNOSTIC_OUTCOME_VALUES },
					},
					'usage_source': {
						optional: false,
						shape: { kind: 'enum', values: CACHE_USAGE_SOURCE_VALUES },
					}
				}
			},
		},
		'cumulative_cost_usd': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'number' } },
		},
		'context_window': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'number' } },
		},
		'step_number': {
			optional: true,
			shape: { kind: 'number' },
		},
		'duration_ms': {
			optional: true,
			shape: { kind: 'number' },
		},
		'request_kind': {
			optional: true,
			shape: { kind: 'enum', values: REQUEST_KIND_VALUES },
		},
		'call_kind': {
			optional: false,
			shape: { kind: 'enum', values: LLM_CALL_KIND_VALUES },
		},
		'has_cost': {
			optional: false,
			shape: { kind: 'boolean' },
		}
	}
} as const;
export const AGENT_WEB_SEARCH_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'phase': {
			optional: false,
			shape: { kind: 'enum', values: WEB_SEARCH_PHASE_VALUES },
		},
		'step_number': {
			optional: false,
			shape: { kind: 'number' },
		},
		'run_id': {
			optional: false,
			shape: { kind: 'number' },
		},
		'call_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'call_index': {
			optional: true,
			shape: { kind: 'number' },
		},
		'action': {
			optional: true,
			shape: { kind: 'string' },
		},
		'result': {
			optional: true,
			shape: { kind: 'unknown' },
		}
	}
} as const;
export const APP_BOOTSTRAP_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'status': {
			optional: false,
			shape: { kind: 'enum', values: BOOTSTRAP_STATUS_VALUES },
		}
	}
} as const;
export const CANCEL_RECORDING_RESPONSE_SHAPE = { kind: 'null' } as const;
export const CANCEL_TOOL_RUN_RESPONSE_SHAPE = { kind: 'boolean' } as const;
export const CHECK_LLM_CONNECTION_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'status': {
			optional: false,
			shape: { kind: 'enum', values: LLM_CONNECTION_STATUS_VALUES },
		},
		'reason': {
			optional: true,
			shape: { kind: 'enum', values: LLM_CONNECTION_FAILURE_REASON_VALUES },
		},
		'provider': {
			optional: false,
			shape: { kind: 'string' },
		},
		'model': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const CHECK_SHELL_AVAILABLE_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'available': {
			optional: false,
			shape: { kind: 'boolean' },
		}
	}
} as const;
export const CLEAR_FACTS_RESPONSE_SHAPE = { kind: 'number' } as const;
export const CLEAR_TOOL_RUN_HISTORY_RESPONSE_SHAPE = { kind: 'number' } as const;
export const CONTINUE_SESSION_RESPONSE_SHAPE = { kind: 'null' } as const;
export const COUNT_SESSION_HISTORY_RESPONSE_SHAPE = { kind: 'number' } as const;
export const COUNT_SESSION_HISTORY_SEARCH_RESPONSE_SHAPE = { kind: 'number' } as const;
export const DELETE_ALL_SESSIONS_RESPONSE_SHAPE = { kind: 'number' } as const;
export const DELETE_FACT_RESPONSE_SHAPE = { kind: 'null' } as const;
export const DELETE_SESSION_RESPONSE_SHAPE = { kind: 'null' } as const;
export const DELETE_TOOL_RUN_RESPONSE_SHAPE = { kind: 'boolean' } as const;
export const DISABLE_AUTOSTART_RESPONSE_SHAPE = { kind: 'null' } as const;
export const DISCARD_SANDBOX_OUTPUT_REVIEW_RESPONSE_SHAPE = { kind: 'boolean' } as const;
export const DISCARD_STAGED_CREDENTIALS_RESPONSE_SHAPE = { kind: 'null' } as const;
export const DISCOVER_MODELS_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'provider': {
			optional: false,
			shape: { kind: 'string' },
		},
		'name': {
			optional: false,
			shape: { kind: 'string' },
		},
		'context_window': {
			optional: false,
			shape: { kind: 'number' },
		},
		'supports_streaming': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'supports_tools': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'supports_vision': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'cost_per_1k_input_tokens': {
			optional: true,
			shape: { kind: 'number' },
		},
		'cost_per_1k_output_tokens': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} } as const;
export const ENABLE_AUTOSTART_RESPONSE_SHAPE = { kind: 'null' } as const;
export const END_SESSION_RESPONSE_SHAPE = { kind: 'null' } as const;
export const EXECUTE_SKILL_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'success': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'output': {
			optional: false,
			shape: { kind: 'unknown' },
		},
		'error': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		}
	}
} as const;
export const EXPORT_SESSION_HISTORY_RESPONSE_SHAPE = { kind: 'string' } as const;
export const FORK_SESSION_FROM_MESSAGE_RESPONSE_SHAPE = { kind: 'string' } as const;
export const GET_API_KEY_STATUS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'models': {
			optional: false,
			shape: { kind: 'record', values: { kind: 'boolean' } },
		},
		'providers': {
			optional: false,
			shape: { kind: 'record', values: { kind: 'boolean' } },
		},
		'stt': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'ocr': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'ocr_secret': {
			optional: false,
			shape: { kind: 'boolean' },
		}
	}
} as const;
export const GET_BOOTSTRAP_STATUS_RESPONSE_SHAPE = { kind: 'enum', values: BOOTSTRAP_STATUS_VALUES } as const;
export const GET_LATEST_SESSION_FOR_RESUME_RESPONSE_SHAPE = { kind: 'nullable', inner: {
	kind: 'object',
	fields: {
		'session': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'input_text': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
					},
					'run_end_reason': {
						optional: true,
						shape: { kind: 'string' },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'updated_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			},
		},
		'messages': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'message_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'role': {
						optional: false,
						shape: { kind: 'enum', values: CANONICAL_ROLE_VALUES },
					},
					'content': {
						optional: false,
						shape: { kind: 'string' },
					},
					'message_type': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'enum', values: TRANSCRIPT_MESSAGE_KIND_VALUES } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'tool_call_id': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'attachments': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'asset_id': {
									optional: true,
									shape: { kind: 'string' },
								},
								'media_type': {
									optional: false,
									shape: { kind: 'string' },
								},
								'data': {
									optional: false,
									shape: { kind: 'string' },
								},
								'filename': {
									optional: true,
									shape: { kind: 'string' },
								},
								'path': {
									optional: true,
									shape: { kind: 'string' },
								},
								'sha256': {
									optional: true,
									shape: { kind: 'string' },
								},
								'size_bytes': {
									optional: true,
									shape: { kind: 'number' },
								},
								'expires_at': {
									optional: true,
									shape: { kind: 'string' },
								},
								'representations': {
									optional: true,
									shape: { kind: 'array', items: {
										kind: 'object',
										fields: {
											'representation': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
											},
											'provenance': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'original' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'derived' },
															},
															'operation': {
																optional: false,
																shape: { kind: 'enum', values: MEDIA_DERIVATION_VALUES },
															},
															'provider': {
																optional: true,
																shape: { kind: 'string' },
															},
															'source_kind': {
																optional: true,
																shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
															}
														}
													}
												] },
											},
											'confidence': {
												optional: true,
												shape: { kind: 'number' },
											},
											'cost': {
												optional: true,
												shape: {
													kind: 'object',
													fields: {
														'estimated_tokens': {
															optional: true,
															shape: { kind: 'number' },
														},
														'estimated_usd': {
															optional: true,
															shape: { kind: 'number' },
														}
													}
												},
											},
											'availability': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'available' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'pending' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'unavailable' },
															},
															'reason': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													}
												] },
											},
											'payload': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'inline_data' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'media_type': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'data': {
																			optional: false,
																			shape: { kind: 'string' },
																		}
																	}
																},
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'text' },
															},
															'value': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'structured' },
															},
															'value': {
																optional: false,
																shape: { kind: 'unknown' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'managed_file_ref' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'asset_id': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'filename': {
																			optional: false,
																			shape: { kind: 'nullable', inner: { kind: 'string' } },
																		}
																	}
																},
															}
														}
													}
												] },
											}
										}
									} },
								},
								'preferred_representation': {
									optional: true,
									shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
								}
							}
						} },
					},
					'media_inputs': {
						optional: true,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'asset': {
									optional: false,
									shape: {
										kind: 'object',
										fields: {
											'asset_id': {
												optional: false,
												shape: { kind: 'string' },
											},
											'content_hash': {
												optional: false,
												shape: { kind: 'string' },
											},
											'media_type': {
												optional: false,
												shape: { kind: 'string' },
											},
											'size_bytes': {
												optional: false,
												shape: { kind: 'number' },
											},
											'filename': {
												optional: true,
												shape: { kind: 'string' },
											},
											'source': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_ASSET_SOURCE_VALUES },
											},
											'lifecycle': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_ASSET_LIFECYCLE_VALUES },
											},
											'expires_at': {
												optional: true,
												shape: { kind: 'string' },
											}
										}
									},
								},
								'representations': {
									optional: false,
									shape: { kind: 'array', items: {
										kind: 'object',
										fields: {
											'representation': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
											},
											'provenance': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'original' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'derived' },
															},
															'operation': {
																optional: false,
																shape: { kind: 'enum', values: MEDIA_DERIVATION_VALUES },
															},
															'provider': {
																optional: true,
																shape: { kind: 'string' },
															},
															'source_kind': {
																optional: true,
																shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
															}
														}
													}
												] },
											},
											'confidence': {
												optional: true,
												shape: { kind: 'number' },
											},
											'cost': {
												optional: true,
												shape: {
													kind: 'object',
													fields: {
														'estimated_tokens': {
															optional: true,
															shape: { kind: 'number' },
														},
														'estimated_usd': {
															optional: true,
															shape: { kind: 'number' },
														}
													}
												},
											},
											'availability': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'available' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'pending' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'unavailable' },
															},
															'reason': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													}
												] },
											},
											'payload': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'inline_data' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'media_type': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'data': {
																			optional: false,
																			shape: { kind: 'string' },
																		}
																	}
																},
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'text' },
															},
															'value': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'structured' },
															},
															'value': {
																optional: false,
																shape: { kind: 'unknown' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'managed_file_ref' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'asset_id': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'filename': {
																			optional: false,
																			shape: { kind: 'nullable', inner: { kind: 'string' } },
																		}
																	}
																},
															}
														}
													}
												] },
											}
										}
									} },
								},
								'preferred_representation': {
									optional: true,
									shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
								}
							}
						} },
					},
					'voice': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			} },
		},
		'steps': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'step_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'step_number': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_index': {
						optional: false,
						shape: { kind: 'number' },
					},
					'thought': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_name': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_input': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_call_id': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'observation': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STEP_STATUS_VALUES },
					},
					'is_high_risk': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'confirmed': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'boolean' } },
					},
					'silent': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'started_at': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'completed_at': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'usage': {
			optional: false,
			shape: { kind: 'nullable', inner: {
				kind: 'object',
				fields: {
					'prompt_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'completion_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'total_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cached_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_creation_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_miss_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_window': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'cost_usd': {
						optional: false,
						shape: { kind: 'number' },
					},
					'has_cost': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			} },
		},
		'llm_usage': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'usage_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'step_number': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'request_kind': {
						optional: false,
						shape: { kind: 'enum', values: REQUEST_KIND_VALUES },
					},
					'call_kind': {
						optional: false,
						shape: { kind: 'enum', values: LLM_CALL_KIND_VALUES },
					},
					'model': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'prompt_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'completion_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'total_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cached_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_creation_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_miss_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_accounting': {
						optional: false,
						shape: { kind: 'enum', values: CACHE_ACCOUNTING_VALUES },
					},
					'cache_diagnostics': {
						optional: true,
						shape: {
							kind: 'object',
							fields: {
								'strategy': {
									optional: false,
									shape: { kind: 'enum', values: PROMPT_CACHE_STRATEGY_VALUES },
								},
								'provider': {
									optional: false,
									shape: { kind: 'string' },
								},
								'key_requested': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'system_split': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'downgraded': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'outcome': {
									optional: false,
									shape: { kind: 'enum', values: CACHE_DIAGNOSTIC_OUTCOME_VALUES },
								},
								'usage_source': {
									optional: false,
									shape: { kind: 'enum', values: CACHE_USAGE_SOURCE_VALUES },
								}
							}
						},
					},
					'context_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_window': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'cost_usd': {
						optional: false,
						shape: { kind: 'number' },
					},
					'has_cost': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'duration_ms': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'interactions': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'request_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'owner': {
						optional: false,
						shape: { kind: 'union', anyOf: [
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'session' },
									},
									'session_id': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'scheduled_tool_run' },
									},
									'tool_run_id': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'app_command' },
									}
								}
							}
						] },
					},
					'kind': {
						optional: false,
						shape: { kind: 'enum', values: INTERACTION_KIND_VALUES },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: INTERACTION_STATUS_VALUES },
					},
					'options': {
						optional: true,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'tool_name': {
						optional: true,
						shape: { kind: 'string' },
					},
					'risk_level': {
						optional: true,
						shape: { kind: 'enum', values: RISK_LEVEL_VALUES },
					},
					'summary': {
						optional: true,
						shape: { kind: 'string' },
					},
					'permission_key': {
						optional: true,
						shape: { kind: 'string' },
					},
					'invocation_step_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'tool_index': {
						optional: true,
						shape: { kind: 'number' },
					},
					'tool_call_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'expires_at': {
						optional: true,
						shape: { kind: 'string' },
					}
				}
			} },
		}
	}
} } as const;
export const GET_LOG_INFO_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'enabled': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'level': {
			optional: false,
			shape: { kind: 'enum', values: LOG_LEVEL_VALUES },
		},
		'path': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		}
	}
} as const;
export const GET_MCP_PROMPT_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'description': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'messages': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'role': {
						optional: false,
						shape: { kind: 'string' },
					},
					'content': {
						optional: false,
						shape: { kind: 'union', anyOf: [
							{
								kind: 'object',
								fields: {
									'type': {
										optional: false,
										shape: { kind: 'literal', value: 'text' },
									},
									'text': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'type': {
										optional: false,
										shape: { kind: 'literal', value: 'image' },
									},
									'data': {
										optional: false,
										shape: { kind: 'string' },
									},
									'mimeType': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'type': {
										optional: false,
										shape: { kind: 'literal', value: 'audio' },
									},
									'data': {
										optional: false,
										shape: { kind: 'string' },
									},
									'mimeType': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'type': {
										optional: false,
										shape: { kind: 'literal', value: 'resource_link' },
									},
									'uri': {
										optional: false,
										shape: { kind: 'string' },
									},
									'name': {
										optional: false,
										shape: { kind: 'string' },
									},
									'title': {
										optional: true,
										shape: { kind: 'string' },
									},
									'description': {
										optional: true,
										shape: { kind: 'string' },
									},
									'mimeType': {
										optional: true,
										shape: { kind: 'string' },
									},
									'size': {
										optional: true,
										shape: { kind: 'number' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'type': {
										optional: false,
										shape: { kind: 'literal', value: 'resource' },
									},
									'resource': {
										optional: false,
										shape: {
											kind: 'object',
											fields: {
												'uri': {
													optional: false,
													shape: { kind: 'string' },
												},
												'mimeType': {
													optional: true,
													shape: { kind: 'string' },
												},
												'text': {
													optional: true,
													shape: { kind: 'string' },
												},
												'blob': {
													optional: true,
													shape: { kind: 'string' },
												}
											}
										},
									}
								}
							}
						] },
					}
				}
			} },
		},
		'ttlMs': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'number' } },
		},
		'cacheScope': {
			optional: false,
			shape: { kind: 'enum', values: MCP_CACHE_SCOPE_VALUES },
		}
	}
} as const;
export const GET_PERFORMANCE_METRICS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'phases': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'count': {
						optional: false,
						shape: { kind: 'number' },
					},
					'total_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'p50_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'p95_ms': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			} },
		},
		'counters': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'turn_starts': {
						optional: false,
						shape: { kind: 'number' },
					},
					'first_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'stream_chunks': {
						optional: false,
						shape: { kind: 'number' },
					},
					'chunk_drops': {
						optional: false,
						shape: { kind: 'number' },
					},
					'checkpoint_pending': {
						optional: false,
						shape: { kind: 'number' },
					},
					'branch_point_failures': {
						optional: false,
						shape: { kind: 'number' },
					},
					'event_boundary_check_failures': {
						optional: false,
						shape: { kind: 'number' },
					},
					'projection_failures': {
						optional: false,
						shape: { kind: 'number' },
					},
					'inbox_ack_failures': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_run_result_retries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_run_result_duplicates': {
						optional: false,
						shape: { kind: 'number' },
					},
					'web_search_drops': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'gauges': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'context_queue_items': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'ui': {
			optional: true,
			shape: {
				kind: 'object',
				fields: {
					'frames': {
						optional: false,
						shape: { kind: 'number' },
					},
					'chunks': {
						optional: false,
						shape: { kind: 'number' },
					},
					'drops': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		}
	}
} as const;
export const GET_RECORDING_STATE_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'is_recording': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'is_toggle': {
			optional: false,
			shape: { kind: 'boolean' },
		}
	}
} as const;
export const GET_SANDBOX_OUTPUT_REVIEW_RESPONSE_SHAPE = { kind: 'nullable', inner: {
	kind: 'object',
	fields: {
		'sessionId': {
			optional: false,
			shape: { kind: 'string' },
		},
		'sourceStepId': {
			optional: false,
			shape: { kind: 'string' },
		},
		'expiresAtUnixMs': {
			optional: false,
			shape: { kind: 'number' },
		},
		'storageChargeBytes': {
			optional: false,
			shape: { kind: 'number' },
		},
		'changes': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'changeIndex': {
						optional: false,
						shape: { kind: 'number' },
					},
					'relativePath': {
						optional: false,
						shape: { kind: 'string' },
					},
					'change': {
						optional: false,
						shape: { kind: 'enum', values: SANDBOX_OUTPUT_REVIEW_CHANGE_KIND_DTO_VALUES },
					},
					'beforeBytes': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'afterBytes': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'beforeSha256': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'afterSha256': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'promoted': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			} },
		}
	}
} } as const;
export const GET_SESSION_FOR_RESUME_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'session': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'input_text': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
					},
					'run_end_reason': {
						optional: true,
						shape: { kind: 'string' },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'updated_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			},
		},
		'messages': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'message_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'role': {
						optional: false,
						shape: { kind: 'enum', values: CANONICAL_ROLE_VALUES },
					},
					'content': {
						optional: false,
						shape: { kind: 'string' },
					},
					'message_type': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'enum', values: TRANSCRIPT_MESSAGE_KIND_VALUES } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'tool_call_id': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'attachments': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'asset_id': {
									optional: true,
									shape: { kind: 'string' },
								},
								'media_type': {
									optional: false,
									shape: { kind: 'string' },
								},
								'data': {
									optional: false,
									shape: { kind: 'string' },
								},
								'filename': {
									optional: true,
									shape: { kind: 'string' },
								},
								'path': {
									optional: true,
									shape: { kind: 'string' },
								},
								'sha256': {
									optional: true,
									shape: { kind: 'string' },
								},
								'size_bytes': {
									optional: true,
									shape: { kind: 'number' },
								},
								'expires_at': {
									optional: true,
									shape: { kind: 'string' },
								},
								'representations': {
									optional: true,
									shape: { kind: 'array', items: {
										kind: 'object',
										fields: {
											'representation': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
											},
											'provenance': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'original' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'derived' },
															},
															'operation': {
																optional: false,
																shape: { kind: 'enum', values: MEDIA_DERIVATION_VALUES },
															},
															'provider': {
																optional: true,
																shape: { kind: 'string' },
															},
															'source_kind': {
																optional: true,
																shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
															}
														}
													}
												] },
											},
											'confidence': {
												optional: true,
												shape: { kind: 'number' },
											},
											'cost': {
												optional: true,
												shape: {
													kind: 'object',
													fields: {
														'estimated_tokens': {
															optional: true,
															shape: { kind: 'number' },
														},
														'estimated_usd': {
															optional: true,
															shape: { kind: 'number' },
														}
													}
												},
											},
											'availability': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'available' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'pending' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'unavailable' },
															},
															'reason': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													}
												] },
											},
											'payload': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'inline_data' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'media_type': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'data': {
																			optional: false,
																			shape: { kind: 'string' },
																		}
																	}
																},
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'text' },
															},
															'value': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'structured' },
															},
															'value': {
																optional: false,
																shape: { kind: 'unknown' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'managed_file_ref' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'asset_id': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'filename': {
																			optional: false,
																			shape: { kind: 'nullable', inner: { kind: 'string' } },
																		}
																	}
																},
															}
														}
													}
												] },
											}
										}
									} },
								},
								'preferred_representation': {
									optional: true,
									shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
								}
							}
						} },
					},
					'media_inputs': {
						optional: true,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'asset': {
									optional: false,
									shape: {
										kind: 'object',
										fields: {
											'asset_id': {
												optional: false,
												shape: { kind: 'string' },
											},
											'content_hash': {
												optional: false,
												shape: { kind: 'string' },
											},
											'media_type': {
												optional: false,
												shape: { kind: 'string' },
											},
											'size_bytes': {
												optional: false,
												shape: { kind: 'number' },
											},
											'filename': {
												optional: true,
												shape: { kind: 'string' },
											},
											'source': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_ASSET_SOURCE_VALUES },
											},
											'lifecycle': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_ASSET_LIFECYCLE_VALUES },
											},
											'expires_at': {
												optional: true,
												shape: { kind: 'string' },
											}
										}
									},
								},
								'representations': {
									optional: false,
									shape: { kind: 'array', items: {
										kind: 'object',
										fields: {
											'representation': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
											},
											'provenance': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'original' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'derived' },
															},
															'operation': {
																optional: false,
																shape: { kind: 'enum', values: MEDIA_DERIVATION_VALUES },
															},
															'provider': {
																optional: true,
																shape: { kind: 'string' },
															},
															'source_kind': {
																optional: true,
																shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
															}
														}
													}
												] },
											},
											'confidence': {
												optional: true,
												shape: { kind: 'number' },
											},
											'cost': {
												optional: true,
												shape: {
													kind: 'object',
													fields: {
														'estimated_tokens': {
															optional: true,
															shape: { kind: 'number' },
														},
														'estimated_usd': {
															optional: true,
															shape: { kind: 'number' },
														}
													}
												},
											},
											'availability': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'available' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'pending' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'unavailable' },
															},
															'reason': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													}
												] },
											},
											'payload': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'inline_data' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'media_type': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'data': {
																			optional: false,
																			shape: { kind: 'string' },
																		}
																	}
																},
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'text' },
															},
															'value': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'structured' },
															},
															'value': {
																optional: false,
																shape: { kind: 'unknown' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'managed_file_ref' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'asset_id': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'filename': {
																			optional: false,
																			shape: { kind: 'nullable', inner: { kind: 'string' } },
																		}
																	}
																},
															}
														}
													}
												] },
											}
										}
									} },
								},
								'preferred_representation': {
									optional: true,
									shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
								}
							}
						} },
					},
					'voice': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			} },
		},
		'steps': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'step_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'step_number': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_index': {
						optional: false,
						shape: { kind: 'number' },
					},
					'thought': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_name': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_input': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_call_id': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'observation': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STEP_STATUS_VALUES },
					},
					'is_high_risk': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'confirmed': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'boolean' } },
					},
					'silent': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'started_at': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'completed_at': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'usage': {
			optional: false,
			shape: { kind: 'nullable', inner: {
				kind: 'object',
				fields: {
					'prompt_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'completion_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'total_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cached_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_creation_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_miss_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_window': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'cost_usd': {
						optional: false,
						shape: { kind: 'number' },
					},
					'has_cost': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			} },
		},
		'llm_usage': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'usage_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'step_number': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'request_kind': {
						optional: false,
						shape: { kind: 'enum', values: REQUEST_KIND_VALUES },
					},
					'call_kind': {
						optional: false,
						shape: { kind: 'enum', values: LLM_CALL_KIND_VALUES },
					},
					'model': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'prompt_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'completion_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'total_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cached_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_creation_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_miss_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_accounting': {
						optional: false,
						shape: { kind: 'enum', values: CACHE_ACCOUNTING_VALUES },
					},
					'cache_diagnostics': {
						optional: true,
						shape: {
							kind: 'object',
							fields: {
								'strategy': {
									optional: false,
									shape: { kind: 'enum', values: PROMPT_CACHE_STRATEGY_VALUES },
								},
								'provider': {
									optional: false,
									shape: { kind: 'string' },
								},
								'key_requested': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'system_split': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'downgraded': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'outcome': {
									optional: false,
									shape: { kind: 'enum', values: CACHE_DIAGNOSTIC_OUTCOME_VALUES },
								},
								'usage_source': {
									optional: false,
									shape: { kind: 'enum', values: CACHE_USAGE_SOURCE_VALUES },
								}
							}
						},
					},
					'context_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_window': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'cost_usd': {
						optional: false,
						shape: { kind: 'number' },
					},
					'has_cost': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'duration_ms': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'interactions': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'request_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'owner': {
						optional: false,
						shape: { kind: 'union', anyOf: [
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'session' },
									},
									'session_id': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'scheduled_tool_run' },
									},
									'tool_run_id': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'app_command' },
									}
								}
							}
						] },
					},
					'kind': {
						optional: false,
						shape: { kind: 'enum', values: INTERACTION_KIND_VALUES },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: INTERACTION_STATUS_VALUES },
					},
					'options': {
						optional: true,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'tool_name': {
						optional: true,
						shape: { kind: 'string' },
					},
					'risk_level': {
						optional: true,
						shape: { kind: 'enum', values: RISK_LEVEL_VALUES },
					},
					'summary': {
						optional: true,
						shape: { kind: 'string' },
					},
					'permission_key': {
						optional: true,
						shape: { kind: 'string' },
					},
					'invocation_step_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'tool_index': {
						optional: true,
						shape: { kind: 'number' },
					},
					'tool_call_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'expires_at': {
						optional: true,
						shape: { kind: 'string' },
					}
				}
			} },
		}
	}
} as const;
export const GET_SESSION_LINEAGE_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'parent_session_id': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'parent': {
			optional: false,
			shape: { kind: 'nullable', inner: {
				kind: 'object',
				fields: {
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'input_text': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
					},
					'run_end_reason': {
						optional: true,
						shape: { kind: 'string' },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'updated_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'children': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'session': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'session_id': {
									optional: false,
									shape: { kind: 'string' },
								},
								'input_text': {
									optional: false,
									shape: { kind: 'string' },
								},
								'title': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'status': {
									optional: false,
									shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
								},
								'run_end_reason': {
									optional: true,
									shape: { kind: 'string' },
								},
								'created_at': {
									optional: false,
									shape: { kind: 'string' },
								},
								'updated_at': {
									optional: false,
									shape: { kind: 'string' },
								}
							}
						},
					},
					'parent_session_id': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		}
	}
} as const;
export const GET_SETTINGS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'default_shell': {
			optional: false,
			shape: { kind: 'enum', values: SHELL_CHOICE_VALUES },
		},
		'llm': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'providers': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'provider': {
									optional: false,
									shape: { kind: 'string' },
								},
								'api_style': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'media_capabilities': {
									optional: true,
									shape: {
										kind: 'object',
										fields: {
											'stt': {
												optional: false,
												shape: { kind: 'boolean' },
											},
											'tts': {
												optional: false,
												shape: { kind: 'boolean' },
											},
											'image_gen': {
												optional: false,
												shape: { kind: 'boolean' },
											}
										}
									},
								},
								'base_url': {
									optional: false,
									shape: { kind: 'string' },
								},
								'api_key_ref': {
									optional: true,
									shape: { kind: 'string' },
								},
								'auth_header_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'auth_header_prefix': {
									optional: false,
									shape: { kind: 'string' },
								},
								'proxy_url': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'no_proxy': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'default_max_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'default_temperature': {
									optional: true,
									shape: { kind: 'number' },
								},
								'default_timeout_secs': {
									optional: true,
									shape: { kind: 'number' },
								},
								'default_timeout_streaming_secs': {
									optional: true,
									shape: { kind: 'number' },
								},
								'default_web_search': {
									optional: true,
									shape: { kind: 'string' },
								}
							}
						} },
					},
					'models': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'id': {
									optional: false,
									shape: { kind: 'string' },
								},
								'provider_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'model': {
									optional: false,
									shape: { kind: 'string' },
								},
								'capabilities': {
									optional: false,
									shape: { kind: 'array', items: { kind: 'enum', values: CAPABILITY_VALUES } },
								},
								'temperature': {
									optional: true,
									shape: { kind: 'number' },
								},
								'context_window': {
									optional: true,
									shape: { kind: 'number' },
								},
								'cost_per_1k_input_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'cost_per_1k_output_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'cost_per_1k_cache_read_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'cost_per_1k_cache_write_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'max_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'reasoning_effort': {
									optional: true,
									shape: { kind: 'string' },
								},
								'web_search': {
									optional: true,
									shape: { kind: 'string' },
								},
								'independent_search': {
									optional: true,
									shape: { kind: 'union', anyOf: [
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'deepseek_search' },
												},
												'count': {
													optional: false,
													shape: { kind: 'number' },
												},
												'need_content': {
													optional: false,
													shape: { kind: 'boolean' },
												},
												'pass_original_question': {
													optional: false,
													shape: { kind: 'boolean' },
												},
												'localization': {
													optional: true,
													shape: { kind: 'string' },
												}
											}
										},
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'perplexity_search' },
												},
												'max_results': {
													optional: false,
													shape: { kind: 'number' },
												},
												'search_type': {
													optional: false,
													shape: { kind: 'enum', values: PERPLEXITY_SEARCH_TYPE_VALUES },
												},
												'recency_filter': {
													optional: false,
													shape: { kind: 'nullable', inner: { kind: 'enum', values: PERPLEXITY_SEARCH_RECENCY_VALUES } },
												},
												'country': {
													optional: true,
													shape: { kind: 'string' },
												},
												'max_tokens': {
													optional: true,
													shape: { kind: 'number' },
												},
												'max_tokens_per_page': {
													optional: true,
													shape: { kind: 'number' },
												},
												'search_language_filter': {
													optional: false,
													shape: { kind: 'array', items: { kind: 'string' } },
												},
												'search_domain_filter': {
													optional: false,
													shape: { kind: 'array', items: { kind: 'string' } },
												},
												'last_updated_after_filter': {
													optional: true,
													shape: { kind: 'string' },
												},
												'last_updated_before_filter': {
													optional: true,
													shape: { kind: 'string' },
												},
												'search_after_date_filter': {
													optional: true,
													shape: { kind: 'string' },
												},
												'search_before_date_filter': {
													optional: true,
													shape: { kind: 'string' },
												}
											}
										},
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'aliyun_ai_search' },
												},
												'endpoint': {
													optional: false,
													shape: { kind: 'string' },
												},
												'workspace_name': {
													optional: false,
													shape: { kind: 'string' },
												},
												'top_k': {
													optional: false,
													shape: { kind: 'number' },
												},
												'query_rewrite': {
													optional: false,
													shape: { kind: 'boolean' },
												},
												'content_type': {
													optional: false,
													shape: { kind: 'enum', values: ALIYUN_SEARCH_CONTENT_TYPE_VALUES },
												},
												'way': {
													optional: false,
													shape: { kind: 'enum', values: ALIYUN_SEARCH_WAY_VALUES },
												},
												'time_range': {
													optional: false,
													shape: { kind: 'enum', values: ALIYUN_SEARCH_TIME_RANGE_VALUES },
												},
												'api_key_ref': {
													optional: true,
													shape: { kind: 'string' },
												}
											}
										},
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'mistral_web_search' },
												},
												'premium': {
													optional: false,
													shape: { kind: 'boolean' },
												}
											}
										},
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'zhipu_web_search' },
												}
											}
										}
									] },
								},
								'reasoning_echo_max_chars': {
									optional: true,
									shape: { kind: 'number' },
								}
							}
						} },
					},
					'request_policies': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'request': {
									optional: false,
									shape: { kind: 'enum', values: REQUEST_KIND_VALUES },
								},
								'primary': {
									optional: false,
									shape: { kind: 'string' },
								}
							}
						} },
					},
					'max_total_duration_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'stream_idle_timeout_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_max_retries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_base_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_factor': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_max_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_jitter': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_concurrent_requests': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'hotkey': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'mode': {
						optional: false,
						shape: { kind: 'enum', values: HOTKEY_MODE_VALUES },
					},
					'key_binding': {
						optional: false,
						shape: { kind: 'string' },
					},
					'mute_hotkey': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					}
				}
			},
		},
		'session': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'max_concurrent': {
						optional: false,
						shape: { kind: 'number' },
					},
					'prompt_history_limit': {
						optional: false,
						shape: { kind: 'number' },
					},
					'history_retention_days': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_steps_per_run': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_steps_per_session': {
						optional: true,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'context_limits': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'compaction_ratio': {
						optional: false,
						shape: { kind: 'number' },
					},
					'compaction_reserve_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'default_context_window': {
						optional: false,
						shape: { kind: 'number' },
					},
					'response_token_floor': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_observation_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_transcript_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_images': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_files': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_image_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_file_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_upload_total_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_image_dim_px': {
						optional: false,
						shape: { kind: 'number' },
					},
					'attachment_image_jpeg_quality': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_max_full_file_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_line_span': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_line_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_summary_input_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_max_list_entries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_byte_read_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_write_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_vision_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'search_snippet_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'search_max_results': {
						optional: false,
						shape: { kind: 'number' },
					},
					'search_max_file_size_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'search_window_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'notification_summary_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_run_result_context_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'partial_checkpoint_min_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'partial_checkpoint_interval_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'fact_infer_interval_steps': {
						optional: false,
						shape: { kind: 'number' },
					},
					'fact_extraction_min_interval_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_known_facts': {
						optional: false,
						shape: { kind: 'number' },
					},
					'sanitize_field_max_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_summary_timeout_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'turn_deadline_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'incomplete_tool_args_retries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'stream_stall_warn_delay_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'reasoning_echo_max_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'background_tool_run_tail_max_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'background_tool_run_output_emit_interval_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_run_terminal_ttl_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'mcp_max_binary_base64_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'mcp_max_sse_buffer_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'skills_max_md_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'skills_max_parse_lines': {
						optional: false,
						shape: { kind: 'number' },
					},
					'skills_max_line_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'self_tool_max_instructions_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'self_tool_max_script_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'network_max_retries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'network_backoff_base_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'network_max_body_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'clipboard_history_entries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'clipboard_history_max_entries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'clipboard_entry_max_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_pending_scheduled_tool_runs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'scheduled_tool_runs_due_horizon_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_running_background_tool_runs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'event_chunk_batch_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'input_ring_buffer_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'embedding_chunk_size': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_tools_per_request': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'memory': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'fact_inference_enabled': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			},
		},
		'security': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'permission_mode': {
						optional: false,
						shape: { kind: 'enum', values: PERMISSION_MODE_VALUES },
					},
					'sandbox_mode': {
						optional: false,
						shape: { kind: 'enum', values: SANDBOX_MODE_VALUES },
					},
					'execution_root': {
						optional: true,
						shape: { kind: 'string' },
					},
					'writable_roots': {
						optional: true,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'network_policy': {
						optional: false,
						shape: { kind: 'enum', values: NETWORK_POLICY_VALUES },
					},
					'encrypt_sensitive': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'permissions': {
						optional: true,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'key': {
									optional: false,
									shape: { kind: 'string' },
								},
								'effect': {
									optional: false,
									shape: { kind: 'enum', values: PERMISSION_EFFECT_VALUES },
								}
							}
						} },
					}
				}
			},
		},
		'media': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'input_strategy': {
						optional: false,
						shape: { kind: 'enum', values: MEDIA_INPUT_STRATEGY_VALUES },
					},
					'audio': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'max_duration_secs': {
									optional: false,
									shape: { kind: 'number' },
								},
								'silence_timeout_ms': {
									optional: false,
									shape: { kind: 'number' },
								},
								'vad_threshold': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					},
					'stt': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'provider_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'mcp_server': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'model': {
									optional: false,
									shape: { kind: 'string' },
								},
								'timeout_secs': {
									optional: false,
									shape: { kind: 'number' },
								},
								'min_confidence': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					},
					'ocr': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'provider': {
									optional: false,
									shape: { kind: 'string' },
								},
								'api_key_ref': {
									optional: true,
									shape: { kind: 'string' },
								},
								'api_secret_ref': {
									optional: true,
									shape: { kind: 'string' },
								},
								'base_url': {
									optional: false,
									shape: { kind: 'string' },
								},
								'timeout_secs': {
									optional: false,
									shape: { kind: 'number' },
								},
								'min_confidence': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					},
					'tts': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'provider_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'model': {
									optional: false,
									shape: { kind: 'string' },
								},
								'voice': {
									optional: false,
									shape: { kind: 'string' },
								},
								'timeout_secs': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					},
					'image_gen': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'provider_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'model': {
									optional: false,
									shape: { kind: 'string' },
								},
								'timeout_secs': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					}
				}
			},
		},
		'skills': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'root': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'enabled': {
						optional: true,
						shape: { kind: 'array', items: { kind: 'string' } },
					}
				}
			},
		},
		'skills_exec': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'venv_root': {
						optional: false,
						shape: { kind: 'string' },
					},
					'work_dir': {
						optional: false,
						shape: { kind: 'string' },
					},
					'timeout_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_output_lines': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'mcp_discovery': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'health_interval_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'reconnect_initial_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'reconnect_max_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'reconnect_max_retries': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'mcp_servers': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'name': {
						optional: false,
						shape: { kind: 'string' },
					},
					'transport': {
						optional: false,
						shape: { kind: 'enum', values: MCP_SERVER_TRANSPORT_VALUES },
					},
					'stdio_execution': {
						optional: false,
						shape: { kind: 'union', anyOf: [
							{
								kind: 'object',
								fields: {
									'mode': {
										optional: false,
										shape: { kind: 'literal', value: 'host_process' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'mode': {
										optional: false,
										shape: { kind: 'literal', value: 'managed_package' },
									},
									'package_id': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							}
						] },
					},
					'command': {
						optional: false,
						shape: { kind: 'string' },
					},
					'args': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'env_refs': {
						optional: true,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'credential_ref': {
									optional: true,
									shape: { kind: 'string' },
								},
								'has_value': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						} },
					},
					'cwd': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'url': {
						optional: false,
						shape: { kind: 'string' },
					},
					'enabled': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'exposure': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'default': {
									optional: false,
									shape: { kind: 'enum', values: MCP_TOOL_EXPOSURE_VALUES },
								},
								'tools': {
									optional: false,
									shape: { kind: 'record', values: { kind: 'enum', values: MCP_TOOL_EXPOSURE_VALUES } },
								}
							}
						},
					}
				}
			} },
		},
		'notification': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'session_created': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'session_completed': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'session_paused': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'session_resumed': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'session_error': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'permission_requested': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'tool_run_completed': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					}
				}
			},
		},
		'log': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'level': {
						optional: false,
						shape: { kind: 'enum', values: LOG_LEVEL_VALUES },
					},
					'file_enabled': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'file_path': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					}
				}
			},
		},
		'tool_settings': {
			optional: false,
			shape: { kind: 'record', values: {
				kind: 'object',
				fields: {
					'enabled': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'timeout_secs': {
						optional: true,
						shape: { kind: 'number' },
					},
					'max_output_chars': {
						optional: true,
						shape: { kind: 'number' },
					},
					'max_retries': {
						optional: true,
						shape: { kind: 'number' },
					},
					'retry_backoff_secs': {
						optional: true,
						shape: { kind: 'number' },
					},
					'allowed_paths': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'allowed_domains': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'disabled_operations': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'risk_override': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'enum', values: RISK_LEVEL_VALUES } },
					}
				}
			} },
		}
	}
} as const;
export const HOTKEY_CONFLICT_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'binding': {
			optional: false,
			shape: { kind: 'string' },
		},
		'error': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const HOTKEY_REBIND_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'old_binding': {
			optional: false,
			shape: { kind: 'string' },
		},
		'new_binding': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const INTERACTION_REQUESTED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'request_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'session_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'owner': {
			optional: false,
			shape: { kind: 'union', anyOf: [
				{
					kind: 'object',
					fields: {
						'kind': {
							optional: false,
							shape: { kind: 'literal', value: 'session' },
						},
						'session_id': {
							optional: false,
							shape: { kind: 'string' },
						}
					}
				},
				{
					kind: 'object',
					fields: {
						'kind': {
							optional: false,
							shape: { kind: 'literal', value: 'scheduled_tool_run' },
						},
						'tool_run_id': {
							optional: false,
							shape: { kind: 'string' },
						}
					}
				},
				{
					kind: 'object',
					fields: {
						'kind': {
							optional: false,
							shape: { kind: 'literal', value: 'app_command' },
						}
					}
				}
			] },
		},
		'kind': {
			optional: false,
			shape: { kind: 'enum', values: INTERACTION_KIND_VALUES },
		},
		'status': {
			optional: false,
			shape: { kind: 'enum', values: INTERACTION_STATUS_VALUES },
		},
		'options': {
			optional: true,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'tool_name': {
			optional: true,
			shape: { kind: 'string' },
		},
		'risk_level': {
			optional: true,
			shape: { kind: 'enum', values: RISK_LEVEL_VALUES },
		},
		'summary': {
			optional: true,
			shape: { kind: 'string' },
		},
		'permission_key': {
			optional: true,
			shape: { kind: 'string' },
		},
		'invocation_step_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'tool_index': {
			optional: true,
			shape: { kind: 'number' },
		},
		'tool_call_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'created_at': {
			optional: false,
			shape: { kind: 'string' },
		},
		'expires_at': {
			optional: true,
			shape: { kind: 'string' },
		}
	}
} as const;
export const INTERRUPT_SESSION_RESPONSE_SHAPE = { kind: 'null' } as const;
export const IS_AUTOSTART_ENABLED_RESPONSE_SHAPE = { kind: 'boolean' } as const;
export const LIST_BUILTIN_TOOL_MANIFESTS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'tools': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'identity': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'source': {
									optional: false,
									shape: { kind: 'enum', values: TOOL_SOURCE_VALUES },
								},
								'catalog_group': {
									optional: false,
									shape: { kind: 'enum', values: TOOL_CATALOG_GROUP_VALUES },
								},
								'root': {
									optional: false,
									shape: { kind: 'string' },
								},
								'operation': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'stable_name': {
									optional: false,
									shape: { kind: 'string' },
								}
							}
						},
					},
					'model': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'description': {
									optional: false,
									shape: { kind: 'string' },
								},
								'input_schema': {
									optional: false,
									shape: { kind: 'unknown' },
								}
							}
						},
					},
					'policy': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'risk_level': {
									optional: false,
									shape: { kind: 'enum', values: RISK_LEVEL_VALUES },
								},
								'permission_key': {
									optional: false,
									shape: { kind: 'string' },
								},
								'confirmation': {
									optional: false,
									shape: { kind: 'enum', values: CONFIRMATION_REQUIREMENT_VALUES },
								},
								'idempotency': {
									optional: false,
									shape: { kind: 'enum', values: OPERATION_IDEMPOTENCY_VALUES },
								},
								'scope': {
									optional: false,
									shape: { kind: 'enum', values: TOOL_OPERATION_SCOPE_VALUES },
								},
								'concurrency': {
									optional: false,
									shape: { kind: 'enum', values: TOOL_CONCURRENCY_MODE_VALUES },
								},
								'effect': {
									optional: false,
									shape: { kind: 'enum', values: OPERATION_EFFECT_VALUES },
								},
								'data_sensitivity': {
									optional: false,
									shape: { kind: 'enum', values: DATA_SENSITIVITY_VALUES },
								},
								'network_access': {
									optional: false,
									shape: { kind: 'enum', values: NETWORK_ACCESS_VALUES },
								}
							}
						},
					},
					'presentation': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'label': {
									optional: false,
									shape: { kind: 'string' },
								},
								'renderer': {
									optional: false,
									shape: { kind: 'string' },
								},
								'icon': {
									optional: false,
									shape: { kind: 'string' },
								},
								'represented_source': {
									optional: false,
									shape: { kind: 'enum', values: TOOL_SOURCE_VALUES },
								}
							}
						},
					},
					'root_presentation': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'label': {
									optional: false,
									shape: { kind: 'string' },
								},
								'description': {
									optional: false,
									shape: { kind: 'string' },
								},
								'icon': {
									optional: false,
									shape: { kind: 'string' },
								}
							}
						},
					},
					'prompt': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'when_to_use': {
									optional: false,
									shape: { kind: 'string' },
								},
								'when_not_to_use': {
									optional: false,
									shape: { kind: 'string' },
								},
								'key_operations': {
									optional: false,
									shape: { kind: 'array', items: { kind: 'string' } },
								}
							}
						},
					},
					'availability': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'enabled': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'available': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'availability_reason': {
									optional: true,
									shape: { kind: 'string' },
								},
								'requires_connection': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'requires_permission': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					}
				}
			} },
		}
	}
} as const;
export const LIST_FACTS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'facts': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'fact_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'subject': {
						optional: false,
						shape: { kind: 'string' },
					},
					'predicate': {
						optional: false,
						shape: { kind: 'string' },
					},
					'object': {
						optional: false,
						shape: { kind: 'string' },
					},
					'source': {
						optional: false,
						shape: { kind: 'enum', values: MEMORY_FACT_SOURCE_VALUES },
					},
					'confidence': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tags': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'mention_count': {
						optional: false,
						shape: { kind: 'number' },
					},
					'last_seen_at': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'source_ref': {
						optional: false,
						shape: { kind: 'nullable', inner: {
							kind: 'object',
							fields: {
								'message_id': {
									optional: false,
									shape: { kind: 'string' },
								},
								'snippet': {
									optional: false,
									shape: { kind: 'string' },
								}
							}
						} },
					},
					'durability': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			} },
		},
		'next_cursor': {
			optional: false,
			shape: { kind: 'nullable', inner: {
				kind: 'object',
				fields: {
					'createdAt': {
						optional: false,
						shape: { kind: 'string' },
					},
					'factId': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		}
	}
} as const;
export const LIST_MCP_PROMPTS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'prompts': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'name': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: true,
						shape: { kind: 'string' },
					},
					'description': {
						optional: true,
						shape: { kind: 'string' },
					},
					'arguments': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'title': {
									optional: true,
									shape: { kind: 'string' },
								},
								'description': {
									optional: true,
									shape: { kind: 'string' },
								},
								'required': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						} },
					}
				}
			} },
		},
		'ttlMs': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'number' } },
		},
		'cacheScope': {
			optional: false,
			shape: { kind: 'enum', values: MCP_CACHE_SCOPE_VALUES },
		}
	}
} as const;
export const LIST_MCP_RESOURCES_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'resources': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'uri': {
						optional: false,
						shape: { kind: 'string' },
					},
					'name': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: true,
						shape: { kind: 'string' },
					},
					'description': {
						optional: true,
						shape: { kind: 'string' },
					},
					'mimeType': {
						optional: true,
						shape: { kind: 'string' },
					},
					'size': {
						optional: true,
						shape: { kind: 'number' },
					}
				}
			} },
		},
		'ttlMs': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'number' } },
		},
		'cacheScope': {
			optional: false,
			shape: { kind: 'enum', values: MCP_CACHE_SCOPE_VALUES },
		}
	}
} as const;
export const LIST_MCP_SERVERS_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'name': {
			optional: false,
			shape: { kind: 'string' },
		},
		'transport': {
			optional: false,
			shape: { kind: 'enum', values: MCP_SERVER_TRANSPORT_VALUES },
		},
		'stdio_execution': {
			optional: false,
			shape: { kind: 'union', anyOf: [
				{
					kind: 'object',
					fields: {
						'mode': {
							optional: false,
							shape: { kind: 'literal', value: 'host_process' },
						}
					}
				},
				{
					kind: 'object',
					fields: {
						'mode': {
							optional: false,
							shape: { kind: 'literal', value: 'managed_package' },
						},
						'package_id': {
							optional: false,
							shape: { kind: 'string' },
						}
					}
				}
			] },
		},
		'command': {
			optional: false,
			shape: { kind: 'string' },
		},
		'args': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'env': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'cwd': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'url': {
			optional: false,
			shape: { kind: 'string' },
		},
		'enabled': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'target_binding': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'supports_resources': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'supports_prompts': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'exposure': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'default': {
						optional: false,
						shape: { kind: 'enum', values: MCP_TOOL_EXPOSURE_VALUES },
					},
					'tools': {
						optional: false,
						shape: { kind: 'record', values: { kind: 'enum', values: MCP_TOOL_EXPOSURE_VALUES } },
					}
				}
			},
		},
		'status': {
			optional: false,
			shape: { kind: 'union', anyOf: [
				{ kind: 'literal', value: 'Disconnected' },
				{ kind: 'literal', value: 'Connecting' },
				{ kind: 'literal', value: 'Connected' },
				{
					kind: 'object',
					fields: {
						'Offline': {
							optional: false,
							shape: {
								kind: 'object',
								fields: {
									'error': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
						}
					}
				}
			] },
		},
		'tools': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'name': {
						optional: false,
						shape: { kind: 'string' },
					},
					'description': {
						optional: false,
						shape: { kind: 'string' },
					},
					'input_schema': {
						optional: false,
						shape: { kind: 'unknown' },
					}
				}
			} },
		},
		'last_error': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'diagnostic': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'last_seen_at': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'number' } },
		}
	}
} } as const;
export const LIST_PERMISSIONS_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'key': {
			optional: false,
			shape: { kind: 'string' },
		},
		'effect': {
			optional: false,
			shape: { kind: 'enum', values: PERMISSION_EFFECT_VALUES },
		}
	}
} } as const;
export const LIST_RUNTIME_SESSIONS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'sessions': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'input_text': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
					},
					'waiting_reason': {
						optional: true,
						shape: { kind: 'enum', values: SESSION_WAITING_REASON_VALUES },
					}
				}
			} },
		}
	}
} as const;
export const LIST_SESSION_HISTORY_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'input_text': {
			optional: false,
			shape: { kind: 'string' },
		},
		'title': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'status': {
			optional: false,
			shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
		},
		'run_end_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'created_at': {
			optional: false,
			shape: { kind: 'string' },
		},
		'updated_at': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} } as const;
export const LIST_SESSION_PERMISSIONS_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'session_title': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'capability': {
			optional: false,
			shape: { kind: 'string' },
		},
		'target': {
			optional: false,
			shape: { kind: 'enum', values: PERMISSION_TARGET_VALUES },
		},
		'effect': {
			optional: false,
			shape: { kind: 'enum', values: PERMISSION_EFFECT_VALUES },
		}
	}
} } as const;
export const LIST_SKILLS_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'name': {
			optional: false,
			shape: { kind: 'string' },
		},
		'description': {
			optional: false,
			shape: { kind: 'string' },
		},
		'version': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'language': {
			optional: false,
			shape: { kind: 'string' },
		},
		'enabled': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'executable': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'has_script': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'unavailable_reason': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'manifest_error': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		}
	}
} } as const;
export const LIST_TOOL_RUNS_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'tool_run_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'kind': {
			optional: false,
			shape: { kind: 'enum', values: TOOL_RUN_KIND_DTO_VALUES },
		},
		'status': {
			optional: true,
			shape: { kind: 'enum', values: TOOL_RUN_STATUS_VALUES },
		},
		'session_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'source_step_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'started_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'finished_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'due_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'title': {
			optional: true,
			shape: { kind: 'string' },
		},
		'body': {
			optional: true,
			shape: { kind: 'string' },
		},
		'mode': {
			optional: true,
			shape: { kind: 'enum', values: SCHEDULE_MODE_VALUES },
		},
		'command': {
			optional: true,
			shape: { kind: 'string' },
		},
		'output': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'exit_code': {
			optional: true,
			shape: { kind: 'number' },
		},
		'preview': {
			optional: true,
			shape: { kind: 'string' },
		}
	}
} } as const;
export const LIST_TOOL_RUN_HISTORY_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'tool_run_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'kind': {
			optional: false,
			shape: { kind: 'enum', values: TOOL_RUN_KIND_DTO_VALUES },
		},
		'status': {
			optional: true,
			shape: { kind: 'enum', values: TOOL_RUN_STATUS_VALUES },
		},
		'session_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'source_step_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'started_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'finished_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'due_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'title': {
			optional: true,
			shape: { kind: 'string' },
		},
		'body': {
			optional: true,
			shape: { kind: 'string' },
		},
		'mode': {
			optional: true,
			shape: { kind: 'enum', values: SCHEDULE_MODE_VALUES },
		},
		'command': {
			optional: true,
			shape: { kind: 'string' },
		},
		'output': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'exit_code': {
			optional: true,
			shape: { kind: 'number' },
		},
		'preview': {
			optional: true,
			shape: { kind: 'string' },
		}
	}
} } as const;
export const LOG_FRONTEND_ERROR_RESPONSE_SHAPE = { kind: 'null' } as const;
export const MCP_STATUS_CHANGE_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'name': {
			optional: false,
			shape: { kind: 'string' },
		},
		'status': {
			optional: false,
			shape: { kind: 'union', anyOf: [
				{ kind: 'literal', value: 'Disconnected' },
				{ kind: 'literal', value: 'Connecting' },
				{ kind: 'literal', value: 'Connected' },
				{
					kind: 'object',
					fields: {
						'Offline': {
							optional: false,
							shape: {
								kind: 'object',
								fields: {
									'error': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
						}
					}
				}
			] },
		}
	}
} as const;
export const MEMORY_FACT_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'fact_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'subject': {
			optional: false,
			shape: { kind: 'string' },
		},
		'predicate': {
			optional: false,
			shape: { kind: 'string' },
		},
		'object': {
			optional: false,
			shape: { kind: 'string' },
		},
		'source': {
			optional: false,
			shape: { kind: 'enum', values: MEMORY_FACT_SOURCE_VALUES },
		},
		'confidence': {
			optional: false,
			shape: { kind: 'number' },
		},
		'tags': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'created_at': {
			optional: false,
			shape: { kind: 'string' },
		},
		'mention_count': {
			optional: false,
			shape: { kind: 'number' },
		},
		'last_seen_at': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'source_ref': {
			optional: false,
			shape: { kind: 'nullable', inner: {
				kind: 'object',
				fields: {
					'message_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'snippet': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'durability': {
			optional: false,
			shape: { kind: 'number' },
		}
	}
} as const;
export const MEMORY_RECALL_ITEM_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'entity_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'text': {
			optional: false,
			shape: { kind: 'string' },
		},
		'score': {
			optional: false,
			shape: { kind: 'number' },
		},
		'model': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const MUTE_CHANGED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'muted': {
			optional: false,
			shape: { kind: 'boolean' },
		}
	}
} as const;
export const NOTIFICATION_SHOW_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'title': {
			optional: false,
			shape: { kind: 'string' },
		},
		'body': {
			optional: false,
			shape: { kind: 'string' },
		},
		'notification_kind': {
			optional: true,
			shape: { kind: 'enum', values: AGENT_NOTIFICATION_KIND_VALUES },
		},
		'tool_run_kind': {
			optional: true,
			shape: { kind: 'enum', values: TOOL_RUN_KIND_DTO_VALUES },
		},
		'tool_run_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'tool_run_status': {
			optional: true,
			shape: { kind: 'enum', values: TOOL_RUN_COMPLETION_STATUS_DTO_VALUES },
		}
	}
} as const;
export const OPEN_EXTERNAL_RESPONSE_SHAPE = { kind: 'null' } as const;
export const OPEN_SKILLS_DIR_RESPONSE_SHAPE = { kind: 'null' } as const;
export const PICK_EXECUTION_WORKSPACE_DIRECTORY_RESPONSE_SHAPE = { kind: 'nullable', inner: { kind: 'string' } } as const;
export const PICK_SANDBOX_OUTPUT_DESTINATION_RESPONSE_SHAPE = { kind: 'nullable', inner: { kind: 'string' } } as const;
export const PROCESS_TRANSCRIPT_RESPONSE_SHAPE = { kind: 'union', anyOf: [
	{
		kind: 'object',
		fields: {
			'SessionCreated': {
				optional: false,
				shape: {
					kind: 'object',
					fields: {
						'session_id': {
							optional: false,
							shape: { kind: 'string' },
						},
						'message_id': {
							optional: true,
							shape: { kind: 'string' },
						}
					}
				},
			}
		}
	},
	{
		kind: 'object',
		fields: {
			'Supplemented': {
				optional: false,
				shape: {
					kind: 'object',
					fields: {
						'session_id': {
							optional: false,
							shape: { kind: 'string' },
						},
						'message_id': {
							optional: true,
							shape: { kind: 'string' },
						}
					}
				},
			}
		}
	}
] } as const;
export const PROMOTE_SANDBOX_OUTPUT_RESPONSE_SHAPE = { kind: 'null' } as const;
export const READ_LOG_TAIL_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'path': {
			optional: false,
			shape: { kind: 'string' },
		},
		'content': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const READ_MCP_RESOURCE_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'contents': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'uri': {
						optional: false,
						shape: { kind: 'string' },
					},
					'mimeType': {
						optional: true,
						shape: { kind: 'string' },
					},
					'text': {
						optional: true,
						shape: { kind: 'string' },
					},
					'blob': {
						optional: true,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'ttlMs': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'number' } },
		},
		'cacheScope': {
			optional: false,
			shape: { kind: 'enum', values: MCP_CACHE_SCOPE_VALUES },
		}
	}
} as const;
export const RECALL_MEMORY_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'entity_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'text': {
			optional: false,
			shape: { kind: 'string' },
		},
		'score': {
			optional: false,
			shape: { kind: 'number' },
		},
		'model': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} } as const;
export const RECONNECT_MCP_SERVER_RESPONSE_SHAPE = { kind: 'null' } as const;
export const RECORDING_ERROR_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'recording_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'error': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const RECORDING_STARTED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'is_recording': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'recording_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'reason': {
			optional: true,
			shape: { kind: 'enum', values: RECORDING_STOP_REASON_DTO_VALUES },
		},
		'duration_ms': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const RECORDING_STOPPED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'is_recording': {
			optional: false,
			shape: { kind: 'boolean' },
		},
		'recording_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'reason': {
			optional: true,
			shape: { kind: 'enum', values: RECORDING_STOP_REASON_DTO_VALUES },
		},
		'duration_ms': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const RECORDING_VAD_STATUS_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'signal': {
			optional: false,
			shape: { kind: 'enum', values: VAD_SIGNAL_DTO_VALUES },
		},
		'state': {
			optional: false,
			shape: { kind: 'enum', values: VAD_STATE_DTO_VALUES },
		}
	}
} as const;
export const REFRESH_MCP_SERVERS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'added': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'removed': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'updated': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		},
		'failed': {
			optional: false,
			shape: { kind: 'array', items: { kind: 'string' } },
		}
	}
} as const;
export const REFRESH_SKILLS_RESPONSE_SHAPE = { kind: 'null' } as const;
export const REMOVE_MCP_SERVER_RESPONSE_SHAPE = { kind: 'null' } as const;
export const REOPEN_SESSION_RESPONSE_SHAPE = { kind: 'null' } as const;
export const REPORT_STARTUP_UI_READY_RESPONSE_SHAPE = { kind: 'null' } as const;
export const RESET_PERMISSIONS_RESPONSE_SHAPE = { kind: 'null' } as const;
export const RESET_SESSION_PERMISSIONS_RESPONSE_SHAPE = { kind: 'number' } as const;
export const RESET_TOOL_CIRCUITS_RESPONSE_SHAPE = { kind: 'null' } as const;
export const RESOLVE_CONFIRMATION_RESPONSE_SHAPE = { kind: 'enum', values: CONFIRMATION_RESOLUTION_RESULT_VALUES } as const;
export const REVOKE_PERMISSION_RESPONSE_SHAPE = { kind: 'null' } as const;
export const REVOKE_SESSION_PERMISSION_RESPONSE_SHAPE = { kind: 'null' } as const;
export const ROLLBACK_SESSION_RESPONSE_SHAPE = { kind: 'null' } as const;
export const RUNTIME_SESSION_LIST_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'sessions': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'input_text': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
					},
					'waiting_reason': {
						optional: true,
						shape: { kind: 'enum', values: SESSION_WAITING_REASON_VALUES },
					}
				}
			} },
		}
	}
} as const;
export const RUN_MEMORY_MAINTENANCE_RESPONSE_SHAPE = { kind: 'number' } as const;
export const SEARCH_SESSION_HISTORY_FILTERED_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'input_text': {
			optional: false,
			shape: { kind: 'string' },
		},
		'title': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'status': {
			optional: false,
			shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
		},
		'run_end_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'created_at': {
			optional: false,
			shape: { kind: 'string' },
		},
		'updated_at': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} } as const;
export const SEARCH_SESSION_HISTORY_PAGINATED_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'input_text': {
			optional: false,
			shape: { kind: 'string' },
		},
		'title': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'status': {
			optional: false,
			shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
		},
		'run_end_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'created_at': {
			optional: false,
			shape: { kind: 'string' },
		},
		'updated_at': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} } as const;
export const SEARCH_SESSION_HISTORY_RESPONSE_SHAPE = { kind: 'array', items: {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'input_text': {
			optional: false,
			shape: { kind: 'string' },
		},
		'title': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'status': {
			optional: false,
			shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
		},
		'run_end_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'created_at': {
			optional: false,
			shape: { kind: 'string' },
		},
		'updated_at': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} } as const;
export const SESSION_LIFECYCLE_EVENT_PAYLOAD_SHAPE = { kind: 'union', anyOf: [
	{
		kind: 'object',
		fields: {
			'type': {
				optional: false,
				shape: { kind: 'literal', value: 'created' },
			},
			'session_id': {
				optional: false,
				shape: { kind: 'string' },
			},
			'status': {
				optional: false,
				shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
			},
			'waiting_reason': {
				optional: true,
				shape: { kind: 'enum', values: SESSION_WAITING_REASON_VALUES },
			},
			'title': {
				optional: false,
				shape: { kind: 'nullable', inner: { kind: 'string' } },
			}
		}
	},
	{
		kind: 'object',
		fields: {
			'type': {
				optional: false,
				shape: { kind: 'literal', value: 'updated' },
			},
			'session_id': {
				optional: false,
				shape: { kind: 'string' },
			},
			'status': {
				optional: false,
				shape: { kind: 'enum', values: SESSION_UPDATE_STATUS_VALUES },
			},
			'waiting_reason': {
				optional: true,
				shape: { kind: 'enum', values: SESSION_WAITING_REASON_VALUES },
			},
			'title': {
				optional: false,
				shape: { kind: 'string' },
			},
			'reason': {
				optional: true,
				shape: { kind: 'string' },
			}
		}
	},
	{
		kind: 'object',
		fields: {
			'type': {
				optional: false,
				shape: { kind: 'literal', value: 'completed' },
			},
			'session_id': {
				optional: false,
				shape: { kind: 'string' },
			},
			'title': {
				optional: false,
				shape: { kind: 'string' },
			},
			'reason': {
				optional: false,
				shape: { kind: 'string' },
			}
		}
	},
	{
		kind: 'object',
		fields: {
			'type': {
				optional: false,
				shape: { kind: 'literal', value: 'error' },
			},
			'session_id': {
				optional: false,
				shape: { kind: 'string' },
			},
			'title': {
				optional: false,
				shape: { kind: 'string' },
			},
			'error': {
				optional: false,
				shape: { kind: 'string' },
			}
		}
	},
	{
		kind: 'object',
		fields: {
			'type': {
				optional: false,
				shape: { kind: 'literal', value: 'title_updated' },
			},
			'session_id': {
				optional: false,
				shape: { kind: 'string' },
			},
			'title': {
				optional: false,
				shape: { kind: 'string' },
			}
		}
	},
	{
		kind: 'object',
		fields: {
			'type': {
				optional: false,
				shape: { kind: 'literal', value: 'deleted' },
			},
			'session_id': {
				optional: false,
				shape: { kind: 'nullable', inner: { kind: 'string' } },
			}
		}
	}
] } as const;
export const SESSION_LINEAGE_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'parent_session_id': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'parent': {
			optional: false,
			shape: { kind: 'nullable', inner: {
				kind: 'object',
				fields: {
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'input_text': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
					},
					'run_end_reason': {
						optional: true,
						shape: { kind: 'string' },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'updated_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'children': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'session': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'session_id': {
									optional: false,
									shape: { kind: 'string' },
								},
								'input_text': {
									optional: false,
									shape: { kind: 'string' },
								},
								'title': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'status': {
									optional: false,
									shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
								},
								'run_end_reason': {
									optional: true,
									shape: { kind: 'string' },
								},
								'created_at': {
									optional: false,
									shape: { kind: 'string' },
								},
								'updated_at': {
									optional: false,
									shape: { kind: 'string' },
								}
							}
						},
					},
					'parent_session_id': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		}
	}
} as const;
export const SESSION_PERMISSION_GRANT_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'session_title': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'capability': {
			optional: false,
			shape: { kind: 'string' },
		},
		'target': {
			optional: false,
			shape: { kind: 'enum', values: PERMISSION_TARGET_VALUES },
		},
		'effect': {
			optional: false,
			shape: { kind: 'enum', values: PERMISSION_EFFECT_VALUES },
		}
	}
} as const;
export const SESSION_RECORD_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'session_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'input_text': {
			optional: false,
			shape: { kind: 'string' },
		},
		'title': {
			optional: false,
			shape: { kind: 'nullable', inner: { kind: 'string' } },
		},
		'status': {
			optional: false,
			shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
		},
		'run_end_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'created_at': {
			optional: false,
			shape: { kind: 'string' },
		},
		'updated_at': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const SESSION_RESUME_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'session': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'input_text': {
						optional: false,
						shape: { kind: 'string' },
					},
					'title': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STATUS_VALUES },
					},
					'run_end_reason': {
						optional: true,
						shape: { kind: 'string' },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'updated_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			},
		},
		'messages': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'message_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'role': {
						optional: false,
						shape: { kind: 'enum', values: CANONICAL_ROLE_VALUES },
					},
					'content': {
						optional: false,
						shape: { kind: 'string' },
					},
					'message_type': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'enum', values: TRANSCRIPT_MESSAGE_KIND_VALUES } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'tool_call_id': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'attachments': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'asset_id': {
									optional: true,
									shape: { kind: 'string' },
								},
								'media_type': {
									optional: false,
									shape: { kind: 'string' },
								},
								'data': {
									optional: false,
									shape: { kind: 'string' },
								},
								'filename': {
									optional: true,
									shape: { kind: 'string' },
								},
								'path': {
									optional: true,
									shape: { kind: 'string' },
								},
								'sha256': {
									optional: true,
									shape: { kind: 'string' },
								},
								'size_bytes': {
									optional: true,
									shape: { kind: 'number' },
								},
								'expires_at': {
									optional: true,
									shape: { kind: 'string' },
								},
								'representations': {
									optional: true,
									shape: { kind: 'array', items: {
										kind: 'object',
										fields: {
											'representation': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
											},
											'provenance': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'original' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'derived' },
															},
															'operation': {
																optional: false,
																shape: { kind: 'enum', values: MEDIA_DERIVATION_VALUES },
															},
															'provider': {
																optional: true,
																shape: { kind: 'string' },
															},
															'source_kind': {
																optional: true,
																shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
															}
														}
													}
												] },
											},
											'confidence': {
												optional: true,
												shape: { kind: 'number' },
											},
											'cost': {
												optional: true,
												shape: {
													kind: 'object',
													fields: {
														'estimated_tokens': {
															optional: true,
															shape: { kind: 'number' },
														},
														'estimated_usd': {
															optional: true,
															shape: { kind: 'number' },
														}
													}
												},
											},
											'availability': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'available' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'pending' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'unavailable' },
															},
															'reason': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													}
												] },
											},
											'payload': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'inline_data' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'media_type': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'data': {
																			optional: false,
																			shape: { kind: 'string' },
																		}
																	}
																},
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'text' },
															},
															'value': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'structured' },
															},
															'value': {
																optional: false,
																shape: { kind: 'unknown' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'managed_file_ref' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'asset_id': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'filename': {
																			optional: false,
																			shape: { kind: 'nullable', inner: { kind: 'string' } },
																		}
																	}
																},
															}
														}
													}
												] },
											}
										}
									} },
								},
								'preferred_representation': {
									optional: true,
									shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
								}
							}
						} },
					},
					'media_inputs': {
						optional: true,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'asset': {
									optional: false,
									shape: {
										kind: 'object',
										fields: {
											'asset_id': {
												optional: false,
												shape: { kind: 'string' },
											},
											'content_hash': {
												optional: false,
												shape: { kind: 'string' },
											},
											'media_type': {
												optional: false,
												shape: { kind: 'string' },
											},
											'size_bytes': {
												optional: false,
												shape: { kind: 'number' },
											},
											'filename': {
												optional: true,
												shape: { kind: 'string' },
											},
											'source': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_ASSET_SOURCE_VALUES },
											},
											'lifecycle': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_ASSET_LIFECYCLE_VALUES },
											},
											'expires_at': {
												optional: true,
												shape: { kind: 'string' },
											}
										}
									},
								},
								'representations': {
									optional: false,
									shape: { kind: 'array', items: {
										kind: 'object',
										fields: {
											'representation': {
												optional: false,
												shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
											},
											'provenance': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'original' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'derived' },
															},
															'operation': {
																optional: false,
																shape: { kind: 'enum', values: MEDIA_DERIVATION_VALUES },
															},
															'provider': {
																optional: true,
																shape: { kind: 'string' },
															},
															'source_kind': {
																optional: true,
																shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
															}
														}
													}
												] },
											},
											'confidence': {
												optional: true,
												shape: { kind: 'number' },
											},
											'cost': {
												optional: true,
												shape: {
													kind: 'object',
													fields: {
														'estimated_tokens': {
															optional: true,
															shape: { kind: 'number' },
														},
														'estimated_usd': {
															optional: true,
															shape: { kind: 'number' },
														}
													}
												},
											},
											'availability': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'available' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'pending' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'state': {
																optional: false,
																shape: { kind: 'literal', value: 'unavailable' },
															},
															'reason': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													}
												] },
											},
											'payload': {
												optional: false,
												shape: { kind: 'union', anyOf: [
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'inline_data' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'media_type': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'data': {
																			optional: false,
																			shape: { kind: 'string' },
																		}
																	}
																},
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'text' },
															},
															'value': {
																optional: false,
																shape: { kind: 'string' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'structured' },
															},
															'value': {
																optional: false,
																shape: { kind: 'unknown' },
															}
														}
													},
													{
														kind: 'object',
														fields: {
															'kind': {
																optional: false,
																shape: { kind: 'literal', value: 'managed_file_ref' },
															},
															'value': {
																optional: false,
																shape: {
																	kind: 'object',
																	fields: {
																		'asset_id': {
																			optional: false,
																			shape: { kind: 'string' },
																		},
																		'filename': {
																			optional: false,
																			shape: { kind: 'nullable', inner: { kind: 'string' } },
																		}
																	}
																},
															}
														}
													}
												] },
											}
										}
									} },
								},
								'preferred_representation': {
									optional: true,
									shape: { kind: 'enum', values: MEDIA_REPRESENTATION_KIND_VALUES },
								}
							}
						} },
					},
					'voice': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			} },
		},
		'steps': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'step_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'step_number': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_index': {
						optional: false,
						shape: { kind: 'number' },
					},
					'thought': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_name': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_input': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'tool_call_id': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'observation': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: SESSION_STEP_STATUS_VALUES },
					},
					'is_high_risk': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'confirmed': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'boolean' } },
					},
					'silent': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'started_at': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'completed_at': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'usage': {
			optional: false,
			shape: { kind: 'nullable', inner: {
				kind: 'object',
				fields: {
					'prompt_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'completion_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'total_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cached_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_creation_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_miss_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_window': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'cost_usd': {
						optional: false,
						shape: { kind: 'number' },
					},
					'has_cost': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			} },
		},
		'llm_usage': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'usage_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'step_number': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'request_kind': {
						optional: false,
						shape: { kind: 'enum', values: REQUEST_KIND_VALUES },
					},
					'call_kind': {
						optional: false,
						shape: { kind: 'enum', values: LLM_CALL_KIND_VALUES },
					},
					'model': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'prompt_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'completion_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'total_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cached_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_creation_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_miss_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'cache_accounting': {
						optional: false,
						shape: { kind: 'enum', values: CACHE_ACCOUNTING_VALUES },
					},
					'cache_diagnostics': {
						optional: true,
						shape: {
							kind: 'object',
							fields: {
								'strategy': {
									optional: false,
									shape: { kind: 'enum', values: PROMPT_CACHE_STRATEGY_VALUES },
								},
								'provider': {
									optional: false,
									shape: { kind: 'string' },
								},
								'key_requested': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'system_split': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'downgraded': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'outcome': {
									optional: false,
									shape: { kind: 'enum', values: CACHE_DIAGNOSTIC_OUTCOME_VALUES },
								},
								'usage_source': {
									optional: false,
									shape: { kind: 'enum', values: CACHE_USAGE_SOURCE_VALUES },
								}
							}
						},
					},
					'context_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'context_window': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'cost_usd': {
						optional: false,
						shape: { kind: 'number' },
					},
					'has_cost': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'duration_ms': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'number' } },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					}
				}
			} },
		},
		'interactions': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'request_id': {
						optional: false,
						shape: { kind: 'string' },
					},
					'session_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'owner': {
						optional: false,
						shape: { kind: 'union', anyOf: [
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'session' },
									},
									'session_id': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'scheduled_tool_run' },
									},
									'tool_run_id': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'kind': {
										optional: false,
										shape: { kind: 'literal', value: 'app_command' },
									}
								}
							}
						] },
					},
					'kind': {
						optional: false,
						shape: { kind: 'enum', values: INTERACTION_KIND_VALUES },
					},
					'status': {
						optional: false,
						shape: { kind: 'enum', values: INTERACTION_STATUS_VALUES },
					},
					'options': {
						optional: true,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'tool_name': {
						optional: true,
						shape: { kind: 'string' },
					},
					'risk_level': {
						optional: true,
						shape: { kind: 'enum', values: RISK_LEVEL_VALUES },
					},
					'summary': {
						optional: true,
						shape: { kind: 'string' },
					},
					'permission_key': {
						optional: true,
						shape: { kind: 'string' },
					},
					'invocation_step_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'tool_index': {
						optional: true,
						shape: { kind: 'number' },
					},
					'tool_call_id': {
						optional: true,
						shape: { kind: 'string' },
					},
					'created_at': {
						optional: false,
						shape: { kind: 'string' },
					},
					'expires_at': {
						optional: true,
						shape: { kind: 'string' },
					}
				}
			} },
		}
	}
} as const;
export const SETTINGS_RESPONSE_SHAPE = {
	kind: 'object',
	fields: {
		'default_shell': {
			optional: false,
			shape: { kind: 'enum', values: SHELL_CHOICE_VALUES },
		},
		'llm': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'providers': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'provider': {
									optional: false,
									shape: { kind: 'string' },
								},
								'api_style': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'media_capabilities': {
									optional: true,
									shape: {
										kind: 'object',
										fields: {
											'stt': {
												optional: false,
												shape: { kind: 'boolean' },
											},
											'tts': {
												optional: false,
												shape: { kind: 'boolean' },
											},
											'image_gen': {
												optional: false,
												shape: { kind: 'boolean' },
											}
										}
									},
								},
								'base_url': {
									optional: false,
									shape: { kind: 'string' },
								},
								'api_key_ref': {
									optional: true,
									shape: { kind: 'string' },
								},
								'auth_header_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'auth_header_prefix': {
									optional: false,
									shape: { kind: 'string' },
								},
								'proxy_url': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'no_proxy': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'default_max_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'default_temperature': {
									optional: true,
									shape: { kind: 'number' },
								},
								'default_timeout_secs': {
									optional: true,
									shape: { kind: 'number' },
								},
								'default_timeout_streaming_secs': {
									optional: true,
									shape: { kind: 'number' },
								},
								'default_web_search': {
									optional: true,
									shape: { kind: 'string' },
								}
							}
						} },
					},
					'models': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'id': {
									optional: false,
									shape: { kind: 'string' },
								},
								'provider_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'model': {
									optional: false,
									shape: { kind: 'string' },
								},
								'capabilities': {
									optional: false,
									shape: { kind: 'array', items: { kind: 'enum', values: CAPABILITY_VALUES } },
								},
								'temperature': {
									optional: true,
									shape: { kind: 'number' },
								},
								'context_window': {
									optional: true,
									shape: { kind: 'number' },
								},
								'cost_per_1k_input_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'cost_per_1k_output_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'cost_per_1k_cache_read_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'cost_per_1k_cache_write_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'max_tokens': {
									optional: true,
									shape: { kind: 'number' },
								},
								'reasoning_effort': {
									optional: true,
									shape: { kind: 'string' },
								},
								'web_search': {
									optional: true,
									shape: { kind: 'string' },
								},
								'independent_search': {
									optional: true,
									shape: { kind: 'union', anyOf: [
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'deepseek_search' },
												},
												'count': {
													optional: false,
													shape: { kind: 'number' },
												},
												'need_content': {
													optional: false,
													shape: { kind: 'boolean' },
												},
												'pass_original_question': {
													optional: false,
													shape: { kind: 'boolean' },
												},
												'localization': {
													optional: true,
													shape: { kind: 'string' },
												}
											}
										},
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'perplexity_search' },
												},
												'max_results': {
													optional: false,
													shape: { kind: 'number' },
												},
												'search_type': {
													optional: false,
													shape: { kind: 'enum', values: PERPLEXITY_SEARCH_TYPE_VALUES },
												},
												'recency_filter': {
													optional: false,
													shape: { kind: 'nullable', inner: { kind: 'enum', values: PERPLEXITY_SEARCH_RECENCY_VALUES } },
												},
												'country': {
													optional: true,
													shape: { kind: 'string' },
												},
												'max_tokens': {
													optional: true,
													shape: { kind: 'number' },
												},
												'max_tokens_per_page': {
													optional: true,
													shape: { kind: 'number' },
												},
												'search_language_filter': {
													optional: false,
													shape: { kind: 'array', items: { kind: 'string' } },
												},
												'search_domain_filter': {
													optional: false,
													shape: { kind: 'array', items: { kind: 'string' } },
												},
												'last_updated_after_filter': {
													optional: true,
													shape: { kind: 'string' },
												},
												'last_updated_before_filter': {
													optional: true,
													shape: { kind: 'string' },
												},
												'search_after_date_filter': {
													optional: true,
													shape: { kind: 'string' },
												},
												'search_before_date_filter': {
													optional: true,
													shape: { kind: 'string' },
												}
											}
										},
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'aliyun_ai_search' },
												},
												'endpoint': {
													optional: false,
													shape: { kind: 'string' },
												},
												'workspace_name': {
													optional: false,
													shape: { kind: 'string' },
												},
												'top_k': {
													optional: false,
													shape: { kind: 'number' },
												},
												'query_rewrite': {
													optional: false,
													shape: { kind: 'boolean' },
												},
												'content_type': {
													optional: false,
													shape: { kind: 'enum', values: ALIYUN_SEARCH_CONTENT_TYPE_VALUES },
												},
												'way': {
													optional: false,
													shape: { kind: 'enum', values: ALIYUN_SEARCH_WAY_VALUES },
												},
												'time_range': {
													optional: false,
													shape: { kind: 'enum', values: ALIYUN_SEARCH_TIME_RANGE_VALUES },
												},
												'api_key_ref': {
													optional: true,
													shape: { kind: 'string' },
												}
											}
										},
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'mistral_web_search' },
												},
												'premium': {
													optional: false,
													shape: { kind: 'boolean' },
												}
											}
										},
										{
											kind: 'object',
											fields: {
												'provider': {
													optional: false,
													shape: { kind: 'literal', value: 'zhipu_web_search' },
												}
											}
										}
									] },
								},
								'reasoning_echo_max_chars': {
									optional: true,
									shape: { kind: 'number' },
								}
							}
						} },
					},
					'request_policies': {
						optional: false,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'request': {
									optional: false,
									shape: { kind: 'enum', values: REQUEST_KIND_VALUES },
								},
								'primary': {
									optional: false,
									shape: { kind: 'string' },
								}
							}
						} },
					},
					'max_total_duration_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'stream_idle_timeout_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_max_retries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_base_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_factor': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_max_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'retry_jitter': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_concurrent_requests': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'hotkey': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'mode': {
						optional: false,
						shape: { kind: 'enum', values: HOTKEY_MODE_VALUES },
					},
					'key_binding': {
						optional: false,
						shape: { kind: 'string' },
					},
					'mute_hotkey': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					}
				}
			},
		},
		'session': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'max_concurrent': {
						optional: false,
						shape: { kind: 'number' },
					},
					'prompt_history_limit': {
						optional: false,
						shape: { kind: 'number' },
					},
					'history_retention_days': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_steps_per_run': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_steps_per_session': {
						optional: true,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'context_limits': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'compaction_ratio': {
						optional: false,
						shape: { kind: 'number' },
					},
					'compaction_reserve_tokens': {
						optional: false,
						shape: { kind: 'number' },
					},
					'default_context_window': {
						optional: false,
						shape: { kind: 'number' },
					},
					'response_token_floor': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_observation_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_transcript_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_images': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_files': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_image_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_file_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_upload_total_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_attachment_image_dim_px': {
						optional: false,
						shape: { kind: 'number' },
					},
					'attachment_image_jpeg_quality': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_max_full_file_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_line_span': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_line_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_summary_input_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_max_list_entries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_byte_read_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_write_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_vision_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'search_snippet_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'search_max_results': {
						optional: false,
						shape: { kind: 'number' },
					},
					'search_max_file_size_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'search_window_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'notification_summary_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_run_result_context_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'partial_checkpoint_min_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'partial_checkpoint_interval_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'fact_infer_interval_steps': {
						optional: false,
						shape: { kind: 'number' },
					},
					'fact_extraction_min_interval_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_known_facts': {
						optional: false,
						shape: { kind: 'number' },
					},
					'sanitize_field_max_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'file_summary_timeout_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'turn_deadline_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'incomplete_tool_args_retries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'stream_stall_warn_delay_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'reasoning_echo_max_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'background_tool_run_tail_max_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'background_tool_run_output_emit_interval_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'tool_run_terminal_ttl_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'mcp_max_binary_base64_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'mcp_max_sse_buffer_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'skills_max_md_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'skills_max_parse_lines': {
						optional: false,
						shape: { kind: 'number' },
					},
					'skills_max_line_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'self_tool_max_instructions_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'self_tool_max_script_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'network_max_retries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'network_backoff_base_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'network_max_body_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'clipboard_history_entries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'clipboard_history_max_entries': {
						optional: false,
						shape: { kind: 'number' },
					},
					'clipboard_entry_max_chars': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_pending_scheduled_tool_runs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'scheduled_tool_runs_due_horizon_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_running_background_tool_runs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'event_chunk_batch_max_bytes': {
						optional: false,
						shape: { kind: 'number' },
					},
					'input_ring_buffer_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'embedding_chunk_size': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_tools_per_request': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'memory': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'fact_inference_enabled': {
						optional: false,
						shape: { kind: 'boolean' },
					}
				}
			},
		},
		'security': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'permission_mode': {
						optional: false,
						shape: { kind: 'enum', values: PERMISSION_MODE_VALUES },
					},
					'sandbox_mode': {
						optional: false,
						shape: { kind: 'enum', values: SANDBOX_MODE_VALUES },
					},
					'execution_root': {
						optional: true,
						shape: { kind: 'string' },
					},
					'writable_roots': {
						optional: true,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'network_policy': {
						optional: false,
						shape: { kind: 'enum', values: NETWORK_POLICY_VALUES },
					},
					'encrypt_sensitive': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'permissions': {
						optional: true,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'key': {
									optional: false,
									shape: { kind: 'string' },
								},
								'effect': {
									optional: false,
									shape: { kind: 'enum', values: PERMISSION_EFFECT_VALUES },
								}
							}
						} },
					}
				}
			},
		},
		'media': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'input_strategy': {
						optional: false,
						shape: { kind: 'enum', values: MEDIA_INPUT_STRATEGY_VALUES },
					},
					'audio': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'max_duration_secs': {
									optional: false,
									shape: { kind: 'number' },
								},
								'silence_timeout_ms': {
									optional: false,
									shape: { kind: 'number' },
								},
								'vad_threshold': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					},
					'stt': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'provider_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'mcp_server': {
									optional: false,
									shape: { kind: 'nullable', inner: { kind: 'string' } },
								},
								'model': {
									optional: false,
									shape: { kind: 'string' },
								},
								'timeout_secs': {
									optional: false,
									shape: { kind: 'number' },
								},
								'min_confidence': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					},
					'ocr': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'provider': {
									optional: false,
									shape: { kind: 'string' },
								},
								'api_key_ref': {
									optional: true,
									shape: { kind: 'string' },
								},
								'api_secret_ref': {
									optional: true,
									shape: { kind: 'string' },
								},
								'base_url': {
									optional: false,
									shape: { kind: 'string' },
								},
								'timeout_secs': {
									optional: false,
									shape: { kind: 'number' },
								},
								'min_confidence': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					},
					'tts': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'provider_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'model': {
									optional: false,
									shape: { kind: 'string' },
								},
								'voice': {
									optional: false,
									shape: { kind: 'string' },
								},
								'timeout_secs': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					},
					'image_gen': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'provider_name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'model': {
									optional: false,
									shape: { kind: 'string' },
								},
								'timeout_secs': {
									optional: false,
									shape: { kind: 'number' },
								}
							}
						},
					}
				}
			},
		},
		'skills': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'root': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'enabled': {
						optional: true,
						shape: { kind: 'array', items: { kind: 'string' } },
					}
				}
			},
		},
		'skills_exec': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'venv_root': {
						optional: false,
						shape: { kind: 'string' },
					},
					'work_dir': {
						optional: false,
						shape: { kind: 'string' },
					},
					'timeout_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'max_output_lines': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'mcp_discovery': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'health_interval_secs': {
						optional: false,
						shape: { kind: 'number' },
					},
					'reconnect_initial_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'reconnect_max_ms': {
						optional: false,
						shape: { kind: 'number' },
					},
					'reconnect_max_retries': {
						optional: false,
						shape: { kind: 'number' },
					}
				}
			},
		},
		'mcp_servers': {
			optional: false,
			shape: { kind: 'array', items: {
				kind: 'object',
				fields: {
					'name': {
						optional: false,
						shape: { kind: 'string' },
					},
					'transport': {
						optional: false,
						shape: { kind: 'enum', values: MCP_SERVER_TRANSPORT_VALUES },
					},
					'stdio_execution': {
						optional: false,
						shape: { kind: 'union', anyOf: [
							{
								kind: 'object',
								fields: {
									'mode': {
										optional: false,
										shape: { kind: 'literal', value: 'host_process' },
									}
								}
							},
							{
								kind: 'object',
								fields: {
									'mode': {
										optional: false,
										shape: { kind: 'literal', value: 'managed_package' },
									},
									'package_id': {
										optional: false,
										shape: { kind: 'string' },
									}
								}
							}
						] },
					},
					'command': {
						optional: false,
						shape: { kind: 'string' },
					},
					'args': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'env_refs': {
						optional: true,
						shape: { kind: 'array', items: {
							kind: 'object',
							fields: {
								'name': {
									optional: false,
									shape: { kind: 'string' },
								},
								'credential_ref': {
									optional: true,
									shape: { kind: 'string' },
								},
								'has_value': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						} },
					},
					'cwd': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					},
					'url': {
						optional: false,
						shape: { kind: 'string' },
					},
					'enabled': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'exposure': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'default': {
									optional: false,
									shape: { kind: 'enum', values: MCP_TOOL_EXPOSURE_VALUES },
								},
								'tools': {
									optional: false,
									shape: { kind: 'record', values: { kind: 'enum', values: MCP_TOOL_EXPOSURE_VALUES } },
								}
							}
						},
					}
				}
			} },
		},
		'notification': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'session_created': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'session_completed': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'session_paused': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'session_resumed': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'session_error': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'permission_requested': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					},
					'tool_run_completed': {
						optional: false,
						shape: {
							kind: 'object',
							fields: {
								'in_app': {
									optional: false,
									shape: { kind: 'boolean' },
								},
								'windows': {
									optional: false,
									shape: { kind: 'boolean' },
								}
							}
						},
					}
				}
			},
		},
		'log': {
			optional: false,
			shape: {
				kind: 'object',
				fields: {
					'level': {
						optional: false,
						shape: { kind: 'enum', values: LOG_LEVEL_VALUES },
					},
					'file_enabled': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'file_path': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'string' } },
					}
				}
			},
		},
		'tool_settings': {
			optional: false,
			shape: { kind: 'record', values: {
				kind: 'object',
				fields: {
					'enabled': {
						optional: false,
						shape: { kind: 'boolean' },
					},
					'timeout_secs': {
						optional: true,
						shape: { kind: 'number' },
					},
					'max_output_chars': {
						optional: true,
						shape: { kind: 'number' },
					},
					'max_retries': {
						optional: true,
						shape: { kind: 'number' },
					},
					'retry_backoff_secs': {
						optional: true,
						shape: { kind: 'number' },
					},
					'allowed_paths': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'allowed_domains': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'disabled_operations': {
						optional: false,
						shape: { kind: 'array', items: { kind: 'string' } },
					},
					'risk_override': {
						optional: false,
						shape: { kind: 'nullable', inner: { kind: 'enum', values: RISK_LEVEL_VALUES } },
					}
				}
			} },
		}
	}
} as const;
export const SET_HOTKEY_CAPTURE_ACTIVE_RESPONSE_SHAPE = { kind: 'null' } as const;
export const SET_REASONING_EFFORT_RESPONSE_SHAPE = { kind: 'null' } as const;
export const SET_SKILL_ENABLED_RESPONSE_SHAPE = { kind: 'null' } as const;
export const SET_TOOL_ENABLED_RESPONSE_SHAPE = { kind: 'null' } as const;
export const SET_WEB_SEARCH_RESPONSE_SHAPE = { kind: 'null' } as const;
export const SKILLS_STATUS_CHANGE_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'op': {
			optional: false,
			shape: { kind: 'enum', values: SKILLS_STATUS_OPERATION_VALUES },
		}
	}
} as const;
export const STAGE_INDEPENDENT_SEARCH_CREDENTIAL_RESPONSE_SHAPE = { kind: 'string' } as const;
export const STAGE_OCR_CREDENTIAL_RESPONSE_SHAPE = { kind: 'string' } as const;
export const STAGE_PROVIDER_CREDENTIAL_RESPONSE_SHAPE = { kind: 'string' } as const;
export const START_RECORDING_RESPONSE_SHAPE = { kind: 'null' } as const;
export const STOP_RECORDING_RESPONSE_SHAPE = { kind: 'null' } as const;
export const SWITCH_MODEL_RESPONSE_SHAPE = { kind: 'null' } as const;
export const TOGGLE_MCP_SERVER_RESPONSE_SHAPE = { kind: 'null' } as const;
export const TOOL_RUN_CREATED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'tool_run_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'kind': {
			optional: false,
			shape: { kind: 'enum', values: TOOL_RUN_KIND_DTO_VALUES },
		},
		'status': {
			optional: true,
			shape: { kind: 'enum', values: TOOL_RUN_STATUS_VALUES },
		},
		'session_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'source_step_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'started_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'finished_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'due_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'title': {
			optional: true,
			shape: { kind: 'string' },
		},
		'body': {
			optional: true,
			shape: { kind: 'string' },
		},
		'mode': {
			optional: true,
			shape: { kind: 'enum', values: SCHEDULE_MODE_VALUES },
		},
		'command': {
			optional: true,
			shape: { kind: 'string' },
		},
		'output': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'exit_code': {
			optional: true,
			shape: { kind: 'number' },
		},
		'preview': {
			optional: true,
			shape: { kind: 'string' },
		}
	}
} as const;
export const TOOL_RUN_FINISHED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'tool_run_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'kind': {
			optional: false,
			shape: { kind: 'enum', values: TOOL_RUN_KIND_DTO_VALUES },
		},
		'status': {
			optional: true,
			shape: { kind: 'enum', values: TOOL_RUN_STATUS_VALUES },
		},
		'session_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'source_step_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'started_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'finished_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'due_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'title': {
			optional: true,
			shape: { kind: 'string' },
		},
		'body': {
			optional: true,
			shape: { kind: 'string' },
		},
		'mode': {
			optional: true,
			shape: { kind: 'enum', values: SCHEDULE_MODE_VALUES },
		},
		'command': {
			optional: true,
			shape: { kind: 'string' },
		},
		'output': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'exit_code': {
			optional: true,
			shape: { kind: 'number' },
		},
		'preview': {
			optional: true,
			shape: { kind: 'string' },
		}
	}
} as const;
export const TOOL_RUN_OUTPUT_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'tool_run_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'kind': {
			optional: false,
			shape: { kind: 'enum', values: TOOL_RUN_KIND_DTO_VALUES },
		},
		'status': {
			optional: true,
			shape: { kind: 'enum', values: TOOL_RUN_STATUS_VALUES },
		},
		'session_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'source_step_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'started_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'finished_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'due_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'title': {
			optional: true,
			shape: { kind: 'string' },
		},
		'body': {
			optional: true,
			shape: { kind: 'string' },
		},
		'mode': {
			optional: true,
			shape: { kind: 'enum', values: SCHEDULE_MODE_VALUES },
		},
		'command': {
			optional: true,
			shape: { kind: 'string' },
		},
		'output': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'exit_code': {
			optional: true,
			shape: { kind: 'number' },
		},
		'preview': {
			optional: true,
			shape: { kind: 'string' },
		}
	}
} as const;
export const TOOL_RUN_UPDATED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'tool_run_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'kind': {
			optional: false,
			shape: { kind: 'enum', values: TOOL_RUN_KIND_DTO_VALUES },
		},
		'status': {
			optional: true,
			shape: { kind: 'enum', values: TOOL_RUN_STATUS_VALUES },
		},
		'session_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'source_step_id': {
			optional: true,
			shape: { kind: 'string' },
		},
		'started_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'finished_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'due_at': {
			optional: true,
			shape: { kind: 'string' },
		},
		'title': {
			optional: true,
			shape: { kind: 'string' },
		},
		'body': {
			optional: true,
			shape: { kind: 'string' },
		},
		'mode': {
			optional: true,
			shape: { kind: 'enum', values: SCHEDULE_MODE_VALUES },
		},
		'command': {
			optional: true,
			shape: { kind: 'string' },
		},
		'output': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error': {
			optional: true,
			shape: { kind: 'string' },
		},
		'error_reason': {
			optional: true,
			shape: { kind: 'string' },
		},
		'exit_code': {
			optional: true,
			shape: { kind: 'number' },
		},
		'preview': {
			optional: true,
			shape: { kind: 'string' },
		}
	}
} as const;
export const TRANSCRIPTION_ERROR_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'recording_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'error': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const TRANSCRIPTION_RESULT_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'recording_id': {
			optional: false,
			shape: { kind: 'string' },
		},
		'text': {
			optional: false,
			shape: { kind: 'string' },
		},
		'duration_ms': {
			optional: false,
			shape: { kind: 'number' },
		},
		'confidence': {
			optional: true,
			shape: { kind: 'number' },
		}
	}
} as const;
export const TRANSCRIPTION_STARTED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'recording_id': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const TRAY_STATUS_CHANGED_EVENT_PAYLOAD_SHAPE = {
	kind: 'object',
	fields: {
		'status': {
			optional: false,
			shape: { kind: 'enum', values: TRAY_STATUS_EVENT_VALUE_VALUES },
		},
		'tooltip': {
			optional: false,
			shape: { kind: 'string' },
		}
	}
} as const;
export const UPDATE_MCP_SERVER_RESPONSE_SHAPE = { kind: 'null' } as const;
export const UPDATE_SESSION_TITLE_RESPONSE_SHAPE = { kind: 'null' } as const;
export const UPDATE_SETTINGS_RESPONSE_SHAPE = { kind: 'null' } as const;

export const TAURI_COMMAND_RESPONSE_SHAPES = {
	add_fact: ADD_FACT_RESPONSE_SHAPE,
	add_mcp_server: ADD_MCP_SERVER_RESPONSE_SHAPE,
	cancel_recording: CANCEL_RECORDING_RESPONSE_SHAPE,
	cancel_tool_run: CANCEL_TOOL_RUN_RESPONSE_SHAPE,
	check_llm_connection: CHECK_LLM_CONNECTION_RESPONSE_SHAPE,
	check_shell_available: CHECK_SHELL_AVAILABLE_RESPONSE_SHAPE,
	clear_facts: CLEAR_FACTS_RESPONSE_SHAPE,
	clear_tool_run_history: CLEAR_TOOL_RUN_HISTORY_RESPONSE_SHAPE,
	continue_session: CONTINUE_SESSION_RESPONSE_SHAPE,
	count_session_history: COUNT_SESSION_HISTORY_RESPONSE_SHAPE,
	count_session_history_search: COUNT_SESSION_HISTORY_SEARCH_RESPONSE_SHAPE,
	delete_all_sessions: DELETE_ALL_SESSIONS_RESPONSE_SHAPE,
	delete_fact: DELETE_FACT_RESPONSE_SHAPE,
	delete_session: DELETE_SESSION_RESPONSE_SHAPE,
	delete_tool_run: DELETE_TOOL_RUN_RESPONSE_SHAPE,
	disable_autostart: DISABLE_AUTOSTART_RESPONSE_SHAPE,
	discard_sandbox_output_review: DISCARD_SANDBOX_OUTPUT_REVIEW_RESPONSE_SHAPE,
	discard_staged_credentials: DISCARD_STAGED_CREDENTIALS_RESPONSE_SHAPE,
	discover_models: DISCOVER_MODELS_RESPONSE_SHAPE,
	enable_autostart: ENABLE_AUTOSTART_RESPONSE_SHAPE,
	end_session: END_SESSION_RESPONSE_SHAPE,
	execute_skill: EXECUTE_SKILL_RESPONSE_SHAPE,
	export_session_history: EXPORT_SESSION_HISTORY_RESPONSE_SHAPE,
	fork_session_from_message: FORK_SESSION_FROM_MESSAGE_RESPONSE_SHAPE,
	get_api_key_status: GET_API_KEY_STATUS_RESPONSE_SHAPE,
	get_bootstrap_status: GET_BOOTSTRAP_STATUS_RESPONSE_SHAPE,
	get_latest_session_for_resume: GET_LATEST_SESSION_FOR_RESUME_RESPONSE_SHAPE,
	get_log_info: GET_LOG_INFO_RESPONSE_SHAPE,
	get_mcp_prompt: GET_MCP_PROMPT_RESPONSE_SHAPE,
	get_performance_metrics: GET_PERFORMANCE_METRICS_RESPONSE_SHAPE,
	get_recording_state: GET_RECORDING_STATE_RESPONSE_SHAPE,
	get_sandbox_output_review: GET_SANDBOX_OUTPUT_REVIEW_RESPONSE_SHAPE,
	get_session_for_resume: GET_SESSION_FOR_RESUME_RESPONSE_SHAPE,
	get_session_lineage: GET_SESSION_LINEAGE_RESPONSE_SHAPE,
	get_settings: GET_SETTINGS_RESPONSE_SHAPE,
	interrupt_session: INTERRUPT_SESSION_RESPONSE_SHAPE,
	is_autostart_enabled: IS_AUTOSTART_ENABLED_RESPONSE_SHAPE,
	list_builtin_tool_manifests: LIST_BUILTIN_TOOL_MANIFESTS_RESPONSE_SHAPE,
	list_facts: LIST_FACTS_RESPONSE_SHAPE,
	list_mcp_prompts: LIST_MCP_PROMPTS_RESPONSE_SHAPE,
	list_mcp_resources: LIST_MCP_RESOURCES_RESPONSE_SHAPE,
	list_mcp_servers: LIST_MCP_SERVERS_RESPONSE_SHAPE,
	list_permissions: LIST_PERMISSIONS_RESPONSE_SHAPE,
	list_runtime_sessions: LIST_RUNTIME_SESSIONS_RESPONSE_SHAPE,
	list_session_history: LIST_SESSION_HISTORY_RESPONSE_SHAPE,
	list_session_permissions: LIST_SESSION_PERMISSIONS_RESPONSE_SHAPE,
	list_skills: LIST_SKILLS_RESPONSE_SHAPE,
	list_tool_run_history: LIST_TOOL_RUN_HISTORY_RESPONSE_SHAPE,
	list_tool_runs: LIST_TOOL_RUNS_RESPONSE_SHAPE,
	log_frontend_error: LOG_FRONTEND_ERROR_RESPONSE_SHAPE,
	open_external: OPEN_EXTERNAL_RESPONSE_SHAPE,
	open_skills_dir: OPEN_SKILLS_DIR_RESPONSE_SHAPE,
	pick_execution_workspace_directory: PICK_EXECUTION_WORKSPACE_DIRECTORY_RESPONSE_SHAPE,
	pick_sandbox_output_destination: PICK_SANDBOX_OUTPUT_DESTINATION_RESPONSE_SHAPE,
	process_transcript: PROCESS_TRANSCRIPT_RESPONSE_SHAPE,
	promote_sandbox_output: PROMOTE_SANDBOX_OUTPUT_RESPONSE_SHAPE,
	read_log_tail: READ_LOG_TAIL_RESPONSE_SHAPE,
	read_mcp_resource: READ_MCP_RESOURCE_RESPONSE_SHAPE,
	recall_memory: RECALL_MEMORY_RESPONSE_SHAPE,
	reconnect_mcp_server: RECONNECT_MCP_SERVER_RESPONSE_SHAPE,
	refresh_mcp_servers: REFRESH_MCP_SERVERS_RESPONSE_SHAPE,
	refresh_skills: REFRESH_SKILLS_RESPONSE_SHAPE,
	remove_mcp_server: REMOVE_MCP_SERVER_RESPONSE_SHAPE,
	reopen_session: REOPEN_SESSION_RESPONSE_SHAPE,
	report_startup_ui_ready: REPORT_STARTUP_UI_READY_RESPONSE_SHAPE,
	reset_permissions: RESET_PERMISSIONS_RESPONSE_SHAPE,
	reset_session_permissions: RESET_SESSION_PERMISSIONS_RESPONSE_SHAPE,
	reset_tool_circuits: RESET_TOOL_CIRCUITS_RESPONSE_SHAPE,
	resolve_confirmation: RESOLVE_CONFIRMATION_RESPONSE_SHAPE,
	revoke_permission: REVOKE_PERMISSION_RESPONSE_SHAPE,
	revoke_session_permission: REVOKE_SESSION_PERMISSION_RESPONSE_SHAPE,
	rollback_session: ROLLBACK_SESSION_RESPONSE_SHAPE,
	run_memory_maintenance: RUN_MEMORY_MAINTENANCE_RESPONSE_SHAPE,
	search_session_history: SEARCH_SESSION_HISTORY_RESPONSE_SHAPE,
	search_session_history_filtered: SEARCH_SESSION_HISTORY_FILTERED_RESPONSE_SHAPE,
	search_session_history_paginated: SEARCH_SESSION_HISTORY_PAGINATED_RESPONSE_SHAPE,
	set_hotkey_capture_active: SET_HOTKEY_CAPTURE_ACTIVE_RESPONSE_SHAPE,
	set_reasoning_effort: SET_REASONING_EFFORT_RESPONSE_SHAPE,
	set_skill_enabled: SET_SKILL_ENABLED_RESPONSE_SHAPE,
	set_tool_enabled: SET_TOOL_ENABLED_RESPONSE_SHAPE,
	set_web_search: SET_WEB_SEARCH_RESPONSE_SHAPE,
	stage_independent_search_credential: STAGE_INDEPENDENT_SEARCH_CREDENTIAL_RESPONSE_SHAPE,
	stage_ocr_credential: STAGE_OCR_CREDENTIAL_RESPONSE_SHAPE,
	stage_provider_credential: STAGE_PROVIDER_CREDENTIAL_RESPONSE_SHAPE,
	start_recording: START_RECORDING_RESPONSE_SHAPE,
	stop_recording: STOP_RECORDING_RESPONSE_SHAPE,
	switch_model: SWITCH_MODEL_RESPONSE_SHAPE,
	toggle_mcp_server: TOGGLE_MCP_SERVER_RESPONSE_SHAPE,
	update_mcp_server: UPDATE_MCP_SERVER_RESPONSE_SHAPE,
	update_session_title: UPDATE_SESSION_TITLE_RESPONSE_SHAPE,
	update_settings: UPDATE_SETTINGS_RESPONSE_SHAPE,
} as const;

export const IPC_EVENT_PAYLOAD_SHAPES = {
	'session:lifecycle': SESSION_LIFECYCLE_EVENT_PAYLOAD_SHAPE,
	'recording:started': RECORDING_STARTED_EVENT_PAYLOAD_SHAPE,
	'recording:stopped': RECORDING_STOPPED_EVENT_PAYLOAD_SHAPE,
	'recording:vad_status': RECORDING_VAD_STATUS_EVENT_PAYLOAD_SHAPE,
	'recording:error': RECORDING_ERROR_EVENT_PAYLOAD_SHAPE,
	'transcription:started': TRANSCRIPTION_STARTED_EVENT_PAYLOAD_SHAPE,
	'transcription:result': TRANSCRIPTION_RESULT_EVENT_PAYLOAD_SHAPE,
	'transcription:error': TRANSCRIPTION_ERROR_EVENT_PAYLOAD_SHAPE,
	'tool_run:created': TOOL_RUN_CREATED_EVENT_PAYLOAD_SHAPE,
	'tool_run:updated': TOOL_RUN_UPDATED_EVENT_PAYLOAD_SHAPE,
	'tool_run:output': TOOL_RUN_OUTPUT_EVENT_PAYLOAD_SHAPE,
	'tool_run:finished': TOOL_RUN_FINISHED_EVENT_PAYLOAD_SHAPE,
	'app:bootstrap': APP_BOOTSTRAP_EVENT_PAYLOAD_SHAPE,
	'tray:status_changed': TRAY_STATUS_CHANGED_EVENT_PAYLOAD_SHAPE,
	'mute:changed': MUTE_CHANGED_EVENT_PAYLOAD_SHAPE,
	'mcp:status_change': MCP_STATUS_CHANGE_EVENT_PAYLOAD_SHAPE,
	'skills:status_change': SKILLS_STATUS_CHANGE_EVENT_PAYLOAD_SHAPE,
	'interaction:requested': INTERACTION_REQUESTED_EVENT_PAYLOAD_SHAPE,
	'hotkey:conflict': HOTKEY_CONFLICT_EVENT_PAYLOAD_SHAPE,
	'hotkey:rebind': HOTKEY_REBIND_EVENT_PAYLOAD_SHAPE,
	'llm:config_changed': { kind: 'null' },
	'agent:thought': AGENT_THOUGHT_EVENT_PAYLOAD_SHAPE,
	'agent:tool_call': AGENT_TOOL_CALL_EVENT_PAYLOAD_SHAPE,
	'agent:observation': AGENT_OBSERVATION_EVENT_PAYLOAD_SHAPE,
	'agent:thought_chunk': AGENT_THOUGHT_CHUNK_EVENT_PAYLOAD_SHAPE,
	'agent:reasoning_chunk': AGENT_REASONING_CHUNK_EVENT_PAYLOAD_SHAPE,
	'agent:tool_call_chunk': AGENT_TOOL_CALL_CHUNK_EVENT_PAYLOAD_SHAPE,
	'agent:stream_reset': AGENT_STREAM_RESET_EVENT_PAYLOAD_SHAPE,
	'agent:media_plan': AGENT_MEDIA_PLAN_EVENT_PAYLOAD_SHAPE,
	'agent:web_search': AGENT_WEB_SEARCH_EVENT_PAYLOAD_SHAPE,
	'agent:stream_stalled': AGENT_STREAM_STALLED_EVENT_PAYLOAD_SHAPE,
	'agent:supplement': AGENT_SUPPLEMENT_EVENT_PAYLOAD_SHAPE,
	'agent:compaction': AGENT_COMPACTION_EVENT_PAYLOAD_SHAPE,
	'agent:usage': AGENT_USAGE_EVENT_PAYLOAD_SHAPE,
	'agent:tool_output': AGENT_TOOL_OUTPUT_EVENT_PAYLOAD_SHAPE,
	'notification:show': NOTIFICATION_SHOW_EVENT_PAYLOAD_SHAPE,
} as const;

export const AGENT_METRICS_PHASE_COUNT = 12 as const;
export const MAX_FACTS_IN_EXTRACTION_PROMPT = 64 as const;


export interface TauriCommandMap {
	add_fact: { request: { subject: string; predicate: string; object: string; tags?: string[] | null }; response: MemoryFactResponse };
	add_mcp_server: { request: { config: McpServerConfigInput }; response: void };
	cancel_recording: { request: undefined; response: void };
	cancel_tool_run: { request: { toolRunId: string; kind: ToolRunKindDtoInput }; response: boolean };
	check_llm_connection: { request: undefined; response: LlmConnectionReport };
	check_shell_available: { request: { shell: ShellChoiceInput }; response: ShellAvailability };
	clear_facts: { request: undefined; response: number };
	clear_tool_run_history: { request: undefined; response: number };
	continue_session: { request: { sessionId: string }; response: void };
	count_session_history: { request: undefined; response: number };
	count_session_history_search: { request: { query: string }; response: number };
	delete_all_sessions: { request: undefined; response: number };
	delete_fact: { request: { factId: string }; response: void };
	delete_session: { request: { sessionId: string }; response: void };
	delete_tool_run: { request: { toolRunId: string }; response: boolean };
	disable_autostart: { request: undefined; response: void };
	discard_sandbox_output_review: { request: { sessionId: string; sourceStepId: string }; response: boolean };
	discard_staged_credentials: { request: undefined; response: void };
	discover_models: { request: { baseUrl: string; apiKey: string; providerName?: string | null; requestKind?: RequestKindInput | null; authHeaderName?: string | null; authHeaderPrefix?: string | null; skipAuth?: boolean | null; proxyUrl?: string | null; noProxy?: string | null }; response: ModelInfo[] };
	enable_autostart: { request: undefined; response: void };
	end_session: { request: { sessionId: string }; response: void };
	execute_skill: { request: { name: string; params: unknown }; response: SkillExecutionResponse };
	export_session_history: { request: { startDate?: string | null; endDate?: string | null; status?: SessionHistoryStatusFilterInput | null }; response: string };
	fork_session_from_message: { request: { sessionId: string; messageId: string }; response: string };
	get_api_key_status: { request: undefined; response: ApiKeyStatus };
	get_bootstrap_status: { request: undefined; response: BootstrapStatus };
	get_latest_session_for_resume: { request: undefined; response: SessionResumeResponse | null };
	get_log_info: { request: undefined; response: LogInfo };
	get_mcp_prompt: { request: { name: string; targetBinding: string; promptName: string; arguments: Record<string, string> }; response: McpPrompt };
	get_performance_metrics: { request: { ui?: UiMetricsSnapshotInput | null }; response: MetricsSnapshot };
	get_recording_state: { request: undefined; response: RecordingStatus };
	get_sandbox_output_review: { request: { sessionId: string; sourceStepId: string }; response: SandboxOutputReviewManifestDto | null };
	get_session_for_resume: { request: { sessionId: string }; response: SessionResumeResponse };
	get_session_lineage: { request: { sessionId: string }; response: SessionLineageResponse };
	get_settings: { request: undefined; response: Settings };
	interrupt_session: { request: { sessionId: string }; response: void };
	is_autostart_enabled: { request: undefined; response: boolean };
	list_builtin_tool_manifests: { request: undefined; response: BuiltinToolManifestListResponse };
	list_facts: { request: { source?: MemoryFactSourceInput | null; cursor?: MemoryFactCursorInput | null; limit?: number | null }; response: MemoryFactPageResponse };
	list_mcp_prompts: { request: { name: string; targetBinding: string; forceRefresh: boolean }; response: McpPromptList };
	list_mcp_resources: { request: { name: string; targetBinding: string; forceRefresh: boolean }; response: McpResourceList };
	list_mcp_servers: { request: undefined; response: McpServerSnapshot[] };
	list_permissions: { request: undefined; response: StoredPermission[] };
	list_runtime_sessions: { request: undefined; response: RuntimeSessionListResponse };
	list_session_history: { request: { limit: number; offset: number }; response: SessionRecordDto[] };
	list_session_permissions: { request: undefined; response: SessionPermissionGrant[] };
	list_skills: { request: undefined; response: SkillInfo[] };
	list_tool_run_history: { request: { kind?: ToolRunKindDtoInput | null; limit?: number | null; sessionId?: string | null }; response: ToolRunEvent[] };
	list_tool_runs: { request: undefined; response: ToolRunEvent[] };
	log_frontend_error: { request: { failureKind: FrontendFailureKindInput }; response: void };
	open_external: { request: { target: string }; response: void };
	open_skills_dir: { request: undefined; response: void };
	pick_execution_workspace_directory: { request: undefined; response: string | null };
	pick_sandbox_output_destination: { request: { suggestedName: string }; response: string | null };
	process_transcript: { request: { transcript: string; activeSessionId?: string | null; attachments?: MessageAttachmentInput[] | null; voice?: boolean | null; recordingId?: string | null; deliveryMode?: InputDeliveryModeInput | null }; response: ProcessResult };
	promote_sandbox_output: { request: { sessionId: string; sourceStepId: string; changeIndex: number; destination: string }; response: void };
	read_log_tail: { request: { maxLines?: number | null }; response: LogTail };
	read_mcp_resource: { request: { name: string; targetBinding: string; uri: string; forceRefresh: boolean }; response: McpResourceRead };
	recall_memory: { request: { query: string; kind?: MemoryEntityKindInput | null; limit?: number | null }; response: MemoryRecallItem[] };
	reconnect_mcp_server: { request: { name: string }; response: void };
	refresh_mcp_servers: { request: undefined; response: McpRefreshResult };
	refresh_skills: { request: undefined; response: void };
	remove_mcp_server: { request: { name: string }; response: void };
	reopen_session: { request: { sessionId: string }; response: void };
	report_startup_ui_ready: { request: undefined; response: void };
	reset_permissions: { request: undefined; response: void };
	reset_session_permissions: { request: undefined; response: number };
	reset_tool_circuits: { request: undefined; response: void };
	resolve_confirmation: { request: { owner: InteractionOwnerInput; requestId: string; effect: PermissionEffectInput; scope: PermissionScopeInput; target: PermissionTargetInput }; response: ConfirmationResolutionResult };
	revoke_permission: { request: { key: string }; response: void };
	revoke_session_permission: { request: { sessionId: string; capability: string }; response: void };
	rollback_session: { request: { sessionId: string; targetStep: number; pause?: boolean | null; targetMessageId?: string | null }; response: void };
	run_memory_maintenance: { request: undefined; response: number };
	search_session_history: { request: { query: string }; response: SessionRecordDto[] };
	search_session_history_filtered: { request: { query?: string | null; status?: SessionHistoryStatusFilterInput | null; startDate?: string | null; endDate?: string | null; limit?: number | null; offset?: number | null }; response: SessionRecordDto[] };
	search_session_history_paginated: { request: { query: string; limit: number; offset: number }; response: SessionRecordDto[] };
	set_hotkey_capture_active: { request: { active: boolean }; response: void };
	set_reasoning_effort: { request: { requestKind: RequestKindInput; effort?: ReasoningEffortSelectionInput | null }; response: void };
	set_skill_enabled: { request: { name: string; enabled: boolean }; response: void };
	set_tool_enabled: { request: { name: string; enabled: boolean }; response: void };
	set_web_search: { request: { requestKind: RequestKindInput; mode?: WebSearchModeInput | null }; response: void };
	stage_independent_search_credential: { request: { modelId: string; apiKey: string }; response: string };
	stage_ocr_credential: { request: { field: OcrCredentialFieldInput; value: string }; response: string };
	stage_provider_credential: { request: { providerName: string; apiKey: string }; response: string };
	start_recording: { request: undefined; response: void };
	stop_recording: { request: undefined; response: void };
	switch_model: { request: { requestKind: RequestKindInput; modelConfigId: string }; response: void };
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
