import { invoke } from '$lib/tauri.ts';
import { parseMcpRefreshResult, parseMcpServerSnapshots } from './contracts/mcp.ts';
import type { McpRefreshResultView, McpServerSnapshotView } from './contracts/mcp.ts';
import { parseSkillExecutionResponse, parseSkillInfoList } from './contracts/skills.ts';
import type { SkillInfoView } from './contracts/skills.ts';
import { parseBuiltinToolManifestList } from './contracts/tools.ts';
import type {
	ReconnectMcpServerRequest,
	RemoveMcpServerRequest,
	McpServerConfig,
	McpServerConfigInput,
	SetSkillEnabledRequest,
	SetToolEnabledRequest,
	ToggleMcpServerRequest,
	BuiltinToolManifestListView,
	ExecuteSkillRequest,
	SkillExecutionResponse,
	UpdateMcpServerRequest,
} from './contracts/tools.ts';

/** Validate the list envelope; individual manifests are parsed by their catalog owner. */
export function listBuiltinToolManifests(): Promise<BuiltinToolManifestListView> {
	return invoke('list_builtin_tool_manifests').then((value: unknown) =>
		parseBuiltinToolManifestList(value),
	);
}

/** Read MCP catalog snapshots using the current Rust status variants. */
export function listMcpServers(): Promise<McpServerSnapshotView[]> {
	return invoke('list_mcp_servers').then((value: unknown) => parseMcpServerSnapshots(value));
}

/** Read the current Skill metadata projection. */
export function listSkills(): Promise<SkillInfoView[]> {
	return invoke('list_skills').then((value: unknown) => parseSkillInfoList(value));
}

/** Clear local tool circuit state. */
export function resetToolCircuits(): Promise<void> {
	return invoke('reset_tool_circuits');
}

/** Reconcile live clients against the persisted MCP server configuration. */
export function refreshMcpServers(): Promise<McpRefreshResultView> {
	return invoke('refresh_mcp_servers').then((value: unknown) => parseMcpRefreshResult(value));
}

/** Set the persisted and live Skill enabled state through the admin command. */
export function setSkillEnabled(request: SetSkillEnabledRequest): Promise<void> {
	return invoke('set_skill_enabled', request);
}

/** Execute a Skill through the same backend authorization path used by Agent calls. */
export function executeSkill(request: ExecuteSkillRequest): Promise<SkillExecutionResponse> {
	return invoke('execute_skill', request).then((value: unknown) =>
		parseSkillExecutionResponse(value),
	);
}

/** Re-scan the configured skills directory and rebuild the tool catalog. */
export function refreshSkills(): Promise<void> {
	return invoke('refresh_skills');
}

/** Open the configured Skills root without exposing its filesystem path. */
export function openSkillsDir(): Promise<void> {
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
