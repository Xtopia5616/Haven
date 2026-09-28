/**
 * ToolsView command response contracts.
 *
 * The Rust DTOs own the snake_case wire shapes. Catalog list commands pass
 * responses through as-is; `toolManifest.ts` remains the single projection
 * from `ToolManifestWire` into the builtin tools view model.
 */

/** Dynamic JSON schema supplied by a builtin, Skill, or MCP tool. */
export type ToolSchema = unknown;

export interface ToolManifestIdentityWire {
	source: string;
	catalog_group: string;
	root: string;
	operation: string | null;
	stable_name: string;
	[field: string]: unknown;
}

export interface ToolModelWire {
	name: string;
	description: string;
	input_schema: ToolSchema;
	[field: string]: unknown;
}

export interface ToolPolicyWire {
	risk_level: string;
	permission_key: string;
	confirmation: string;
	idempotency: string;
	scope: string;
	concurrency: string;
	effect: string;
	data_sensitivity: string;
	network_access: string;
	[field: string]: unknown;
}

export interface ToolPresentationWire {
	label: string;
	renderer: string;
	icon: string;
	represented_source: string;
	[field: string]: unknown;
}

export interface ToolRootPresentationWire {
	label: string;
	description: string;
	icon: string;
	[field: string]: unknown;
}

export interface ToolPromptWire {
	when_to_use: string;
	when_not_to_use: string;
	key_operations: string[];
	[field: string]: unknown;
}

export interface ToolAvailabilityWire {
	enabled: boolean;
	available: boolean;
	availability_reason?: string | null;
	requires_connection: boolean;
	requires_permission: boolean;
	[field: string]: unknown;
}

/** Rust `haven_common::tools::ToolManifest` wire projection. */
export interface ToolManifestWire {
	identity: ToolManifestIdentityWire;
	model: ToolModelWire;
	policy: ToolPolicyWire;
	presentation: ToolPresentationWire;
	root_presentation: ToolRootPresentationWire;
	prompt: ToolPromptWire;
	availability: ToolAvailabilityWire;
	[field: string]: unknown;
}

/** Rust `commands::contracts::ToolListResponse` wire projection. */
export interface ToolListResponse {
	tools: ToolManifestWire[];
	[field: string]: unknown;
}

/** Rust `haven_skills::SkillInfo` wire projection. */
export interface SkillInfo {
	name: string;
	description: string;
	version: string | null;
	language: string;
	enabled: boolean;
	root: string;
	has_script: boolean;
	[field: string]: unknown;
}

/** Rust `haven_mcp::McpClientStatus` wire enum. */
export type McpClientStatus =
	'Disconnected' | 'Connecting' | 'Connected' | { Offline: { error: string } };

/** Rust `haven_mcp::McpToolInfo` wire projection. */
export interface McpToolInfo {
	name: string;
	description: string;
	input_schema: ToolSchema;
	[field: string]: unknown;
}

/** Rust `haven_mcp::McpServerSnapshot` wire projection. */
export interface McpServerSnapshot {
	name: string;
	transport: string;
	command: string;
	args: string[];
	env: string[];
	cwd: string | null;
	url: string;
	enabled: boolean;
	status: McpClientStatus;
	tools: McpToolInfo[];
	last_error: string | null;
	diagnostic: string | null;
	last_seen_at: number | null;
	[field: string]: unknown;
}

/** Rust `haven_common::config::McpServerConfig` wire shape. */
export type McpTransport = 'stdio' | 'http';

export interface McpServerConfig {
	name: string;
	transport: McpTransport;
	command: string;
	args: string[];
	env: string[];
	cwd: string | null;
	url: string;
	enabled: boolean;
}

/** Rust `commands::mcp::McpRefreshResult`; status is reconciled by the view. */
export interface McpRefreshResult {
	added: string[];
	removed: string[];
	updated: string[];
	failed: string[];
	[field: string]: unknown;
}

/** Tauri flat command arguments shared by server and toggle operations. */
export interface McpNameRequest {
	name: string;
}

export interface SetEnabledRequest {
	name: string;
	enabled: boolean;
}

export interface ToggleMcpServerRequest {
	name: string;
	enabled: boolean;
}

/** The Rust handler accepts these as the top-level `name` and `config` args. */
export interface UpdateMcpServerRequest {
	name: string;
	config: McpServerConfig;
}
