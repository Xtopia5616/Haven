/**
 * Versioned directory of every Tauri command.
 *
 * Rust handler signatures and Serialize DTOs own IPC shapes. This directory
 * adds only renderer-reviewed boundary and security metadata.
 */

import type { TauriCommandName, TauriCommandRequest } from './generatedCommands.ts';

/** Semantic command aliases derived from Rust handler signatures. */
export type ResolveConfirmationRequest = TauriCommandRequest<'resolve_confirmation'>;
export type ContinueSessionRequest = TauriCommandRequest<'continue_session'>;
export type DeleteSessionRequest = TauriCommandRequest<'delete_session'>;
export type EndSessionRequest = TauriCommandRequest<'end_session'>;
export type GetSessionForResumeRequest = TauriCommandRequest<'get_session_for_resume'>;
export type GetSessionLineageRequest = TauriCommandRequest<'get_session_lineage'>;
export type InterruptSessionRequest = TauriCommandRequest<'interrupt_session'>;
export type ReopenSessionRequest = TauriCommandRequest<'reopen_session'>;
export type RollbackSessionRequest = TauriCommandRequest<'rollback_session'>;
export type UpdateSessionTitleRequest = TauriCommandRequest<'update_session_title'>;
export type SessionHistoryPageRequest = TauriCommandRequest<'list_session_history'>;
export type SessionHistoryFilterRequest = TauriCommandRequest<'search_session_history_filtered'>;
export type SwitchModelRequest = TauriCommandRequest<'switch_model'>;
export type SetReasoningEffortRequest = TauriCommandRequest<'set_reasoning_effort'>;
export type SetWebSearchRequest = TauriCommandRequest<'set_web_search'>;
export type DiscoverModelsRequest = TauriCommandRequest<'discover_models'>;
export type CancelToolRunRequest = TauriCommandRequest<'cancel_tool_run'>;
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
	list_tool_runs: { boundary: 'read', security: 'projected task fields only' },
	cancel_tool_run: { boundary: 'mutate', security: 'kind is enum; cancel only the selected task kind' },
	list_tool_run_history: { boundary: 'read', security: 'optional session filter; limit capped at 200; internal tool args excluded' },
	delete_tool_run: { boundary: 'mutate', security: 'delete one persisted task row by id' },
	clear_tool_run_history: { boundary: 'mutate', security: 'delete terminal task history; preserve live work and undelivered results' },
	open_external: { boundary: 'execute', security: 'http(s) or validated absolute local path only' },
	list_session_history: { boundary: 'read', security: 'read-only session projection' },
	count_session_history: { boundary: 'read', security: 'read-only aggregate' },
	search_session_history_paginated: { boundary: 'read', security: 'parameterized read-only search' },
	count_session_history_search: { boundary: 'read', security: 'parameterized read-only search' },
	search_session_history: { boundary: 'read', security: 'parameterized read-only search' },
	search_session_history_filtered: { boundary: 'read', security: 'bounded page and date-filtered projection' },
	export_session_history: { boundary: 'read', security: 'export contains persisted history only' },
	get_log_info: { boundary: 'read', security: 'path is optional; no environment details' },
	read_log_tail: { boundary: 'read', security: 'bounded tail; file logging must be enabled' },
	log_frontend_error: { boundary: 'mutate', security: 'sanitized user-visible error mirrored into the backend log' },
	get_performance_metrics: { boundary: 'read', security: 'bounded content-free backend counters plus renderer stream counters' },
	list_mcp_servers: { boundary: 'read', security: 'snapshot only; env values redacted; invocation remains gated' },
	reconnect_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; typed native operation reconnects one existing configured server after final version check' },
	refresh_mcp_servers: { boundary: 'execute', security: 'AuthorizationEngine; one batch over persisted config diff and its affected targets; no renderer process arguments' },
	add_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; shared native admin operation validates and persists config' },
	update_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; shared native admin operation validates and reconnects safely' },
	remove_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; shared native admin operation removes client and config' },
	toggle_mcp_server: { boundary: 'execute', security: 'AuthorizationEngine; shared native admin operation connects before enabling' },
	run_memory_maintenance: { boundary: 'mutate', security: 'maintenance path owns purge and embedding cleanup' },
	recall_memory: { boundary: 'read', security: 'bounded and credential-filtered recall' },
	list_facts: { boundary: 'read', security: 'read-only fact projection' },
	add_fact: { boundary: 'mutate', security: 'credential-like predicates and values rejected' },
	delete_fact: { boundary: 'mutate', security: 'delete one fact by id' },
	clear_facts: { boundary: 'mutate', security: 'delete all saved long-term facts and invalidate derived caches' },
	get_api_key_status: { boundary: 'read', security: 'boolean presence only; credentials excluded' },
	check_llm_connection: { boundary: 'read', security: 'status and non-sensitive reason only; no endpoint or provider payload' },
	discover_models: { boundary: 'execute', security: 'providerName is a configured connection name; http(s) endpoint; typed auth scheme for an explicitly entered key; stored keys require a matching configured endpoint; uses configured provider proxy' },
	discover_all_models: { boundary: 'execute', security: 'only configured providers are queried, each with its configured proxy' },
	switch_model: { boundary: 'mutate', security: 'requestKind selects the route; modelId is an assigned, capability-compatible ModelConfig id' },
	set_reasoning_effort: { boundary: 'mutate', security: 'requestKind selects the assigned model before config save' },
	set_web_search: { boundary: 'mutate', security: 'requestKind selects the assigned model; provider capability checked before config save' },
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
	list_runtime_sessions: { boundary: 'read', security: 'resident nonterminal runtime sessions only' },
	get_session_lineage: { boundary: 'read', security: 'parent and direct children of the selected session only' },
	end_session: { boundary: 'mutate', security: 'explicit user termination' },
	interrupt_session: { boundary: 'mutate', security: 'pauses the selected active session without deleting it' },
	resolve_confirmation: { boundary: 'mutate', security: 'owner and request id select one registry; receipt, effect, scope, target and expiry are revalidated' },
	update_session_title: { boundary: 'mutate', security: 'trimmed non-empty title only' },
	delete_session: { boundary: 'mutate', security: 'delete by session id and release runtime state' },
	delete_all_sessions: { boundary: 'mutate', security: 'clears persisted sessions and session trust' },
	rollback_session: { boundary: 'mutate', security: 'event cursor and projection clock rollback' },
	continue_session: { boundary: 'mutate', security: 'resume from saved error snapshot' },
	get_session_for_resume: { boundary: 'read', security: 'session-scoped persisted projection' },
	get_latest_session_for_resume: { boundary: 'read', security: 'most recent persisted session only' },
	get_settings: { boundary: 'read', security: 'config response redacts credentials and MCP environment values' },
	stage_provider_credential: { boundary: 'mutate', security: 'writes provider secret to secure storage and returns only an opaque reference' },
	stage_ocr_credential: { boundary: 'mutate', security: 'writes OCR secret to secure storage and returns only an opaque reference' },
	discard_staged_credentials: { boundary: 'mutate', security: 'deletes staged values not committed by a Settings save' },
	get_bootstrap_status: { boundary: 'read', security: 'status enum only' },
	update_settings: { boundary: 'mutate', security: 'shared loader preserves masked secrets, tool sections, and permission rules' },
	list_permissions: { boundary: 'read', security: 'permanent permission keys/effects only' },
	list_session_permissions: { boundary: 'read', security: 'typed session grants with exact session, capability, target, and effect' },
	revoke_permission: { boundary: 'mutate', security: 'non-empty exact permanent key; session grants are retained' },
	revoke_session_permission: { boundary: 'mutate', security: 'non-empty session id and capability; removes one session grant' },
	reset_permissions: { boundary: 'mutate', security: 'clears permanent rules only; keeps session grants and selected default policy' },
	reset_session_permissions: { boundary: 'mutate', security: 'clears durable session grants only; keeps permanent rules' },
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
	list_builtin_tool_manifests: { boundary: 'read', security: 'tool definition projection; schemas are dynamic extension data' },
	reset_tool_circuits: { boundary: 'mutate', security: 'clears local circuit state only' },
} as const satisfies Record<TauriCommandName, CommandContract>;

export const TAURI_COMMAND_NAMES = Object.keys(TAURI_COMMAND_CONTRACTS) as TauriCommandName[];
