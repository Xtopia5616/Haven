import { invoke } from '$lib/tauri.ts';
import type {
	McpNameRequest,
	McpRefreshResult,
	McpServerConfig,
	McpServerSnapshot,
	SetEnabledRequest,
	SkillInfo,
	ToggleMcpServerRequest,
	ToolListResponse,
	UpdateMcpServerRequest,
} from './contracts/tools.ts';

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isMcpClientStatus(value: unknown): value is McpServerSnapshot['status'] {
	if (value === 'Disconnected' || value === 'Connecting' || value === 'Connected') return true;
	if (!isRecord(value) || Object.keys(value).length !== 1 || !('Offline' in value)) return false;
	const offline = value.Offline;
	return (
		isRecord(offline) && Object.keys(offline).length === 1 && typeof offline.error === 'string'
	);
}

function validateMcpServerSnapshots(value: unknown): McpServerSnapshot[] {
	if (
		!Array.isArray(value) ||
		!value.every((snapshot) => isRecord(snapshot) && isMcpClientStatus(snapshot.status))
	) {
		throw new Error('Invalid MCP server snapshot status');
	}
	return value as McpServerSnapshot[];
}

/** Read builtin tool manifests using their existing Rust wire shape. */
export function getTools(): Promise<ToolListResponse> {
	return invoke('get_tools');
}

/** Read MCP catalog snapshots using the current Rust status variants. */
export function listMcpTools(): Promise<McpServerSnapshot[]> {
	return invoke('list_mcp_tools').then((value: unknown) => validateMcpServerSnapshots(value));
}

/** Read the current Skill metadata projection. */
export function listSkills(): Promise<SkillInfo[]> {
	return invoke('list_skills');
}

/** Clear local tool circuit state. */
export function resetToolCircuits(): Promise<void> {
	return invoke('reset_tool_circuits');
}

/** Reconcile live clients against the persisted MCP server configuration. */
export function refreshMcpServers(): Promise<McpRefreshResult> {
	return invoke('refresh_mcp_servers');
}

/** Set the persisted and live Skill enabled state through the admin command. */
export function setSkillEnabled(request: SetEnabledRequest): Promise<void> {
	return invoke('set_skill_enabled', request);
}

/** Re-scan the configured skills directory and rebuild the tool catalog. */
export function refreshSkills(): Promise<void> {
	return invoke('refresh_skills');
}

/** Open the configured Skills root; the backend returns the resolved path. */
export function openSkillsDir(): Promise<string> {
	return invoke('open_skills_dir');
}

/** Add an MCP server using the Rust config DTO without parsing it again. */
export function addMcpServer(config: McpServerConfig): Promise<void> {
	return invoke('add_mcp_server', { config });
}

/** Update an MCP server using the handler's flat `name` and `config` args. */
export function updateMcpServer(request: UpdateMcpServerRequest): Promise<void> {
	return invoke('update_mcp_server', request);
}

/** Remove a configured MCP server by its name. */
export function removeMcpServer(request: McpNameRequest): Promise<void> {
	return invoke('remove_mcp_server', request);
}

/** Reconnect the selected MCP server. */
export function reconnectMcp(request: McpNameRequest): Promise<void> {
	return invoke('reconnect_mcp', request);
}

/** Enable or disable the selected MCP server. */
export function toggleMcpServer(request: ToggleMcpServerRequest): Promise<void> {
	return invoke('toggle_mcp_server', request);
}

/** Persist and apply an enabled-state change for a builtin tool. */
export function setToolEnabled(request: SetEnabledRequest): Promise<void> {
	return invoke('set_tool_enabled', request);
}
