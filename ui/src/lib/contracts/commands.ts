/**
 * Versioned directory of every Tauri command.
 *
 * Rust handler signatures and Serialize DTOs own IPC shapes. This directory
 * adds only renderer-reviewed boundary and security metadata.
 */

import type { TauriCommandRequest } from './generatedCommands.ts';

/** Semantic command aliases derived from Rust handler signatures. */
export type SessionIdRequest = TauriCommandRequest<'reopen_session'>;
export type ResolveConfirmationRequest = TauriCommandRequest<'resolve_confirmation'>;
export type RollbackSessionRequest = TauriCommandRequest<'rollback_session'>;
export type UpdateSessionTitleRequest = TauriCommandRequest<'update_session_title'>;
export type HistoryPageRequest = TauriCommandRequest<'get_history'>;
export type HistorySearchRequest = TauriCommandRequest<'search_history'>;
export type HistorySearchPageRequest = TauriCommandRequest<'search_history_paginated'>;
export type HistoryFilterRequest = TauriCommandRequest<'search_history_filtered'>;
export type HistoryExportRequest = TauriCommandRequest<'export_history'>;
export type SwitchModelRequest = TauriCommandRequest<'switch_model'>;
export type SetReasoningEffortRequest = TauriCommandRequest<'set_reasoning_effort'>;
export type SetWebSearchRequest = TauriCommandRequest<'set_web_search'>;
export type DiscoverModelsRequest = TauriCommandRequest<'discover_models'>;
export type CancelActionRequest = TauriCommandRequest<'cancel_action'>;
export type RecallMemoryRequest = TauriCommandRequest<'recall_memory'>;
export type ListFactsRequest = TauriCommandRequest<'list_facts'>;
export type AddFactRequest = TauriCommandRequest<'add_fact'>;
export type DeleteFactRequest = TauriCommandRequest<'delete_fact'>;
export type ReadLogTailRequest = TauriCommandRequest<'read_log_tail'>;
export type CheckShellAvailableRequest = TauriCommandRequest<'check_shell_available'>;
export type UiMetricsSnapshot = NonNullable<
  NonNullable<TauriCommandRequest<'get_performance_metrics'>>['ui']
>;

export type CommandBoundary = 'read' | 'mutate' | 'execute';

export interface CommandContract {
	boundary: CommandBoundary;
	security: string;
}

export const TAURI_COMMAND_CONTRACTS = {
	list_actions: { boundary: 'read', security: 'projected task fields only' },
	cancel_action: { boundary: 'mutate', security: 'kind is enum; cancel only the selected task kind' },
	list_action_history: { boundary: 'read', security: 'limit capped at 200; internal tool args excluded' },
	delete_action: { boundary: 'mutate', security: 'delete one persisted task row by id' },
	open_external: { boundary: 'execute', security: 'http(s) or validated absolute local path only' },
	get_history: { boundary: 'read', security: 'read-only session projection' },
	count_history: { boundary: 'read', security: 'read-only aggregate' },
	search_history_paginated: { boundary: 'read', security: 'parameterized read-only search' },
	count_history_search: { boundary: 'read', security: 'parameterized read-only search' },
	search_history: { boundary: 'read', security: 'parameterized read-only search' },
	search_history_filtered: { boundary: 'read', security: 'bounded page and date-filtered projection' },
	export_history: { boundary: 'read', security: 'export contains persisted history only' },
	get_log_info: { boundary: 'read', security: 'path is optional; no environment details' },
	read_log_tail: { boundary: 'read', security: 'bounded tail; file logging must be enabled' },
	log_frontend_error: { boundary: 'mutate', security: 'sanitized user-visible error mirrored into the backend log' },
	get_performance_metrics: { boundary: 'read', security: 'bounded content-free backend counters plus renderer stream counters' },
	list_mcp_tools: { boundary: 'read', security: 'snapshot only; invocation remains gated' },
	reconnect_mcp: { boundary: 'execute', security: 'AuthorizationEngine; typed native operation reconnects one existing configured server after final version check' },
	refresh_mcp_servers: { boundary: 'execute', security: 'AuthorizationEngine; one batch over persisted config diff and its affected targets; no renderer process arguments' },
	mcp_tool_call: { boundary: 'execute', security: 'AuthorizationEngine; direct confirmations are queued and renderer errors are safe' },
	add_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; shared native admin operation validates and persists config' },
	update_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; shared native admin operation validates and reconnects safely' },
	remove_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; shared native admin operation removes client and config' },
	toggle_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; shared native admin operation connects before enabling' },
	run_memory_maintenance: { boundary: 'mutate', security: 'maintenance path owns purge and embedding cleanup' },
	recall_memory: { boundary: 'read', security: 'bounded and credential-filtered recall' },
	list_facts: { boundary: 'read', security: 'read-only fact projection' },
	add_fact: { boundary: 'mutate', security: 'credential-like predicates and values rejected' },
	delete_fact: { boundary: 'mutate', security: 'delete one fact by id' },
	get_api_key_status: { boundary: 'read', security: 'boolean presence only; credentials excluded' },
	check_llm_connection: { boundary: 'read', security: 'status and non-sensitive reason only; no endpoint or provider payload' },
	discover_models: { boundary: 'execute', security: 'http(s) endpoint; typed auth scheme for an explicitly entered key; stored keys require a matching configured endpoint' },
	discover_all_models: { boundary: 'execute', security: 'only configured providers are queried' },
	switch_model: { boundary: 'mutate', security: 'role carries a model id or RequestKind and is validated before config save' },
	set_reasoning_effort: { boundary: 'mutate', security: 'role carries a model id or RequestKind and is validated before config save' },
	set_web_search: { boundary: 'mutate', security: 'provider capability checked before config save' },
	get_recording_state: { boundary: 'read', security: 'state only; no device or provider detail' },
	set_hotkey_capture_active: {
		boundary: 'mutate',
		security: 'transient renderer key-capture state only; not persisted',
	},
	start_recording: { boundary: 'execute', security: 'input pipeline owns capture lifecycle' },
	stop_recording: { boundary: 'execute', security: 'capture stops before asynchronous transcription' },
	cancel_recording: { boundary: 'execute', security: 'cancel clears the in-flight recording id' },
	process_transcript: { boundary: 'execute', security: 'attachment limits and file persistence are enforced' },
	reopen_session: { boundary: 'mutate', security: 'session id selects persisted session' },
	get_sessions: { boundary: 'read', security: 'active session projection' },
	end_session: { boundary: 'mutate', security: 'explicit user termination' },
	interrupt_session: { boundary: 'mutate', security: 'pauses the selected active session without deleting it' },
	resolve_confirmation: { boundary: 'mutate', security: 'effect/scope must match confirmation; deny wins' },
	update_session_title: { boundary: 'mutate', security: 'trimmed non-empty title only' },
	delete_session: { boundary: 'mutate', security: 'delete by session id and release runtime state' },
	clear_history: { boundary: 'mutate', security: 'clears persisted sessions and session trust' },
	rollback_session: { boundary: 'mutate', security: 'event cursor and projection clock rollback' },
	continue_session: { boundary: 'mutate', security: 'resume from saved error snapshot' },
	get_session_for_resume: { boundary: 'read', security: 'session-scoped persisted projection' },
	get_last_conversation: { boundary: 'read', security: 'most recent persisted session only' },
	get_settings: { boundary: 'read', security: 'config response redacts credentials and MCP environment values' },
	stage_provider_credential: { boundary: 'mutate', security: 'writes provider secret to secure storage and returns only an opaque reference' },
	stage_ocr_credential: { boundary: 'mutate', security: 'writes OCR secret to secure storage and returns only an opaque reference' },
	discard_staged_credentials: { boundary: 'mutate', security: 'deletes staged values not committed by a Settings save' },
	get_bootstrap_status: { boundary: 'read', security: 'status enum only' },
	update_settings: { boundary: 'mutate', security: 'shared loader preserves masked secrets, tool sections, and permission rules' },
	list_permissions: { boundary: 'read', security: 'permission keys/effects only' },
	revoke_permission: { boundary: 'mutate', security: 'non-empty exact key; persisted atomically' },
	reset_permissions: { boundary: 'mutate', security: 'clears permanent and session rules; keeps selected default policy' },
	check_shell_available: { boundary: 'read', security: 'availability boolean only' },
	enable_autostart: { boundary: 'execute', security: 'release build only' },
	disable_autostart: { boundary: 'execute', security: 'managed autostart entry only' },
	is_autostart_enabled: { boundary: 'read', security: 'state boolean only' },
	list_skills: { boundary: 'read', security: 'metadata projection' },
	refresh_skills: { boundary: 'execute', security: 'configured skills root scan' },
	set_skill_enabled: { boundary: 'mutate', security: 'AuthorizationEngine; shared native admin operation persists the toggle' },
	set_tool_enabled: { boundary: 'mutate', security: 'AuthorizationEngine; shared native admin operation persists the toggle' },
	open_skills_dir: { boundary: 'execute', security: 'configured skills root only' },
	execute_skill: { boundary: 'execute', security: 'AuthorizationEngine; direct confirmations are queued and renderer errors are safe' },
	get_tools: { boundary: 'read', security: 'tool definition projection; schemas are dynamic extension data' },
	reset_tool_circuits: { boundary: 'mutate', security: 'clears local circuit state only' },
} as const satisfies Record<string, CommandContract>;

export type TauriCommandName = keyof typeof TAURI_COMMAND_CONTRACTS;
export const TAURI_COMMAND_NAMES = Object.keys(TAURI_COMMAND_CONTRACTS) as TauriCommandName[];
