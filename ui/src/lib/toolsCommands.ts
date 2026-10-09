import { invoke } from '$lib/tauri.ts';
import { isMcpClientStatus } from './contracts/mcpClientStatus.ts';
import { isRecord } from './contracts/objectGuards.ts';
import { MCP_TRANSPORT_TYPE_VALUES } from './contracts/generatedCommands.ts';
import type {
	ReconnectMcpServerRequest,
	RemoveMcpServerRequest,
	McpRefreshResult,
	McpServerConfig,
	McpServerConfigInput,
	McpServerSnapshot,
	SetSkillEnabledRequest,
	SetToolEnabledRequest,
	SkillInfo,
	ToggleMcpServerRequest,
	BuiltinToolManifestListResponse,
	ExecuteSkillRequest,
	SkillExecutionResponse,
	UpdateMcpServerRequest,
} from './contracts/tools.ts';

function validateMcpServerSnapshots(value: unknown): McpServerSnapshot[] {
	if (
		!Array.isArray(value) ||
		!value.every((snapshot) => isRecord(snapshot) && isMcpClientStatus(snapshot.status))
	) {
		throw new Error('Invalid MCP server snapshot status');
	}
	if (
		!value.every(
			(snapshot) =>
				isRecord(snapshot) &&
				typeof snapshot.transport === 'string' &&
				(MCP_TRANSPORT_TYPE_VALUES as readonly string[]).includes(snapshot.transport),
		)
	) {
		throw new Error('Invalid MCP server snapshot transport');
	}
	return value as McpServerSnapshot[];
}

/** List builtin tool manifests using their existing Rust wire shape. */
export function listBuiltinToolManifests(): Promise<BuiltinToolManifestListResponse> {
	return invoke('list_builtin_tool_manifests');
}

/** Read MCP catalog snapshots using the current Rust status variants. */
export function listMcpServers(): Promise<McpServerSnapshot[]> {
	return invoke('list_mcp_servers').then((value: unknown) => validateMcpServerSnapshots(value));
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
export function setSkillEnabled(request: SetSkillEnabledRequest): Promise<void> {
	return invoke('set_skill_enabled', request);
}

/** Execute a Skill through the same backend authorization path used by Agent calls. */
export function executeSkill(request: ExecuteSkillRequest): Promise<SkillExecutionResponse> {
	return invoke('execute_skill', request);
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
export function addMcpServer(config: McpServerConfigInput): Promise<void> {
	return invoke('add_mcp_server', { config });
}

/** Update an MCP server using the handler's flat `name` and `config` args. */
export function updateMcpServer(request: UpdateMcpServerRequest): Promise<void> {
	return invoke('update_mcp_server', request);
}

/** Remove a configured MCP server by its name. */
export function removeMcpServer(request: RemoveMcpServerRequest): Promise<void> {
	return invoke('remove_mcp_server', request);
}

/** Reconnect the selected MCP server. */
export function reconnectMcpServer(request: ReconnectMcpServerRequest): Promise<void> {
	return invoke('reconnect_mcp_server', request);
}

/** Enable or disable the selected MCP server. */
export function toggleMcpServer(request: ToggleMcpServerRequest): Promise<void> {
	return invoke('toggle_mcp_server', request);
}

/** Persist and apply an enabled-state change for a builtin tool. */
export function setToolEnabled(request: SetToolEnabledRequest): Promise<void> {
	return invoke('set_tool_enabled', request);
}
