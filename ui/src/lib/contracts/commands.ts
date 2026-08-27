/**
 * Versioned directory of every Tauri command.
 *
 * Tauri keeps the request fields flat on the wire (for example
 * `{ sessionId }`), while this directory gives each command a named request
 * schema and a stable response contract. Rust owns the executable handler
 * signatures; this module is the renderer's exhaustive boundary inventory.
 */

export type CommandBoundary = 'read' | 'mutate' | 'execute';

export interface CommandContract {
	request: string;
	response: string;
	boundary: CommandBoundary;
	security: string;
}

export const TAURI_COMMAND_CONTRACTS = {
	list_actions: { request: '-', response: 'ActionEvent[]', boundary: 'read', security: 'projected task fields only' },
	cancel_action: { request: 'CancelActionRequest', response: 'boolean', boundary: 'mutate', security: 'kind is enum; cancel only the selected task kind' },
	list_action_history: { request: 'ListActionHistoryRequest', response: 'ActionEvent[]', boundary: 'read', security: 'limit capped at 200; internal tool args excluded' },
	delete_action: { request: 'DeleteActionRequest', response: 'boolean', boundary: 'mutate', security: 'delete one persisted task row by id' },
	open_external: { request: 'OpenExternalRequest', response: 'void', boundary: 'execute', security: 'http(s) or validated absolute local path only' },
	get_history: { request: 'HistoryPageRequest', response: 'Session[]', boundary: 'read', security: 'read-only session projection' },
	count_history: { request: '-', response: 'number', boundary: 'read', security: 'read-only aggregate' },
	search_history_paginated: { request: 'HistorySearchPageRequest', response: 'Session[]', boundary: 'read', security: 'parameterized read-only search' },
	count_history_search: { request: 'HistorySearchRequest', response: 'number', boundary: 'read', security: 'parameterized read-only search' },
	search_history: { request: 'HistorySearchRequest', response: 'Session[]', boundary: 'read', security: 'parameterized read-only search' },
	search_history_filtered: { request: 'HistoryFilterRequest', response: 'Session[]', boundary: 'read', security: 'bounded page and date-filtered projection' },
	export_history: { request: 'HistoryExportRequest', response: 'string', boundary: 'read', security: 'export contains persisted history only' },
	get_log_info: { request: '-', response: 'LogInfo', boundary: 'read', security: 'path is optional; no environment details' },
	read_log_tail: { request: 'ReadLogTailRequest', response: 'LogTail', boundary: 'read', security: 'bounded tail; file logging must be enabled' },
	list_mcp_tools: { request: '-', response: 'McpServerSnapshot[]', boundary: 'read', security: 'snapshot only; invocation remains gated' },
	reconnect_mcp: { request: 'McpNameRequest', response: 'void', boundary: 'execute', security: 'server name selects an existing configured client' },
	refresh_mcp_servers: { request: '-', response: 'McpRefreshResult', boundary: 'execute', security: 'reconcile configured clients; no renderer command' },
	mcp_tool_call: { request: 'McpToolCallRequest', response: 'McpToolCallResponse', boundary: 'execute', security: 'MCP adapter name and args pass SafetyGateway' },
	add_mcp_server: { request: 'McpServerConfig', response: 'void', boundary: 'execute', security: 'shared self operation validates and persists config' },
	update_mcp_server: { request: 'UpdateMcpServerRequest', response: 'void', boundary: 'execute', security: 'shared self operation validates and reconnects safely' },
	remove_mcp_server: { request: 'McpNameRequest', response: 'void', boundary: 'execute', security: 'shared self operation removes client and config' },
	toggle_mcp_server: { request: 'ToggleMcpServerRequest', response: 'void', boundary: 'execute', security: 'shared self operation connects before enabling' },
	run_memory_maintenance: { request: '-', response: 'number', boundary: 'mutate', security: 'maintenance path owns purge and embedding cleanup' },
	recall_memory: { request: 'RecallMemoryRequest', response: 'MemoryRecallItem[]', boundary: 'read', security: 'bounded and credential-filtered recall' },
	list_facts: { request: 'ListFactsRequest', response: 'Fact[]', boundary: 'read', security: 'read-only fact projection' },
	add_fact: { request: 'AddFactRequest', response: 'Fact', boundary: 'mutate', security: 'credential-like predicates and values rejected' },
	delete_fact: { request: 'DeleteFactRequest', response: 'void', boundary: 'mutate', security: 'delete one fact by id' },
	get_api_key_status: { request: '-', response: 'ApiKeyStatus', boundary: 'read', security: 'boolean presence only; credentials excluded' },
	check_llm_connection: { request: '-', response: 'string', boundary: 'read', security: 'status only; no provider payload' },
	discover_models: { request: 'DiscoverModelsRequest', response: 'ModelInfo[]', boundary: 'execute', security: 'http(s) endpoint plus stored-key host match' },
	discover_all_models: { request: '-', response: 'Record<string, ModelInfo[]>', boundary: 'execute', security: 'only configured providers are queried' },
	switch_model: { request: 'SwitchModelRequest', response: 'void', boundary: 'mutate', security: 'role slot validated before config save' },
	set_reasoning_effort: { request: 'SetReasoningEffortRequest', response: 'void', boundary: 'mutate', security: 'role slot validated before config save' },
	set_web_search: { request: 'SetWebSearchRequest', response: 'void', boundary: 'mutate', security: 'provider capability checked before config save' },
	get_recording_state: { request: '-', response: 'RecordingState', boundary: 'read', security: 'state only; no device or provider detail' },
	start_recording: { request: '-', response: 'void', boundary: 'execute', security: 'input pipeline owns capture lifecycle' },
	stop_recording: { request: '-', response: 'string', boundary: 'execute', security: 'capture stops before asynchronous transcription' },
	cancel_recording: { request: '-', response: 'void', boundary: 'execute', security: 'cancel clears the in-flight recording id' },
	process_transcript: { request: 'ProcessTranscriptRequest', response: 'ProcessResult', boundary: 'execute', security: 'attachment limits and file persistence are enforced' },
	reopen_session: { request: 'SessionIdRequest', response: 'void', boundary: 'mutate', security: 'session id selects persisted session' },
	get_sessions: { request: '-', response: 'SessionListResponse', boundary: 'read', security: 'active session projection' },
	end_session: { request: 'SessionIdRequest', response: 'void', boundary: 'mutate', security: 'explicit user termination' },
	resolve_confirmation: { request: 'ResolveConfirmationRequest', response: 'void', boundary: 'mutate', security: 'effect/scope must match confirmation; deny wins' },
	update_session_title: { request: 'UpdateSessionTitleRequest', response: 'void', boundary: 'mutate', security: 'trimmed non-empty title only' },
	delete_session: { request: 'SessionIdRequest', response: 'void', boundary: 'mutate', security: 'delete by session id and release runtime state' },
	clear_history: { request: '-', response: 'number', boundary: 'mutate', security: 'clears persisted sessions and session trust' },
	rollback_session: { request: 'RollbackSessionRequest', response: 'void', boundary: 'mutate', security: 'event cursor and projection clock rollback' },
	continue_session: { request: 'SessionIdRequest', response: 'void', boundary: 'mutate', security: 'resume from saved error snapshot' },
	get_session_for_resume: { request: 'SessionIdRequest', response: 'SessionResumeResponse', boundary: 'read', security: 'session-scoped persisted projection' },
	get_last_conversation: { request: '-', response: 'SessionResumeResponse | null', boundary: 'read', security: 'most recent persisted session only' },
	get_settings: { request: '-', response: 'Settings', boundary: 'read', security: 'config response masks credentials' },
	get_bootstrap_status: { request: '-', response: 'string', boundary: 'read', security: 'status enum only' },
	update_settings: { request: 'Settings', response: 'void', boundary: 'mutate', security: 'shared loader preserves masked secrets and tool sections' },
	list_permissions: { request: '-', response: 'StoredPermission[]', boundary: 'read', security: 'permission keys/effects only' },
	revoke_permission: { request: 'RevokePermissionRequest', response: 'void', boundary: 'mutate', security: 'non-empty exact key; persisted atomically' },
	check_shell_available: { request: 'CheckShellAvailableRequest', response: 'ShellAvailability', boundary: 'read', security: 'availability boolean only' },
	enable_autostart: { request: '-', response: 'void', boundary: 'execute', security: 'release build only' },
	disable_autostart: { request: '-', response: 'void', boundary: 'execute', security: 'managed autostart entry only' },
	is_autostart_enabled: { request: '-', response: 'boolean', boundary: 'read', security: 'state boolean only' },
	list_skills: { request: '-', response: 'SkillInfo[]', boundary: 'read', security: 'metadata projection' },
	refresh_skills: { request: '-', response: 'void', boundary: 'execute', security: 'configured skills root scan' },
	set_skill_enabled: { request: 'SetEnabledRequest', response: 'void', boundary: 'mutate', security: 'shared self operation persists the toggle' },
	set_tool_enabled: { request: 'SetEnabledRequest', response: 'void', boundary: 'mutate', security: 'shared self operation persists the toggle' },
	open_skills_dir: { request: '-', response: 'string', boundary: 'execute', security: 'configured skills root only' },
	execute_skill: { request: 'ExecuteSkillRequest', response: 'SkillExecutionResponse', boundary: 'execute', security: 'qualified skill name and params pass SafetyGateway' },
	get_tools: { request: '-', response: 'ToolListResponse', boundary: 'read', security: 'tool definition projection; schemas are dynamic extension data' },
	reset_tool_circuits: { request: '-', response: 'void', boundary: 'mutate', security: 'clears local circuit state only' },
} as const satisfies Record<string, CommandContract>;

export type TauriCommandName = keyof typeof TAURI_COMMAND_CONTRACTS;
export const TAURI_COMMAND_NAMES = Object.keys(TAURI_COMMAND_CONTRACTS) as TauriCommandName[];

