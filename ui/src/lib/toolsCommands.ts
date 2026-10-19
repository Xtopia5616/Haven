import { invoke } from '$lib/tauri.ts';
import { parseMcpRefreshResult, parseMcpServerSnapshots } from './contracts/mcp.ts';
import type { McpRefreshResultView, McpServerSnapshotView } from './contracts/mcp.ts';
import {
	parseMcpPromptList,
	parseMcpResourceList,
	parseMcpResourceTemplateList,
	parseMcpFeatureCallOutcome,
} from './contracts/mcpFeatures.ts';
import type {
	McpPromptListView,
	McpResourceListView,
	McpResourceTemplateListView,
	McpFeatureCallOutcomeView,
	McpElicitationAnswerView,
} from './contracts/mcpFeatures.ts';
import { parseSkillExecutionResponse, parseSkillInfoList } from './contracts/skills.ts';
import { parseBooleanResponse } from './contracts/boolean.ts';
import type { SkillInfoView } from './contracts/skills.ts';
import { parseBuiltinToolManifestList } from './contracts/tools.ts';
import { parseSandboxOutputReview } from './contracts/sandboxReview.ts';
import type { SandboxOutputReviewManifestDto } from './contracts/generatedCommands.ts';
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

/** Discover bounded metadata from the server selected in the current snapshot. */
export function listMcpResources(request: {
	name: string;
	targetBinding: string;
	forceRefresh: boolean;
}): Promise<McpResourceListView> {
	return invoke('list_mcp_resources', request).then((value: unknown) =>
		parseMcpResourceList(value),
	);
}

/** Discover parameterized resource templates for explicit user selection. */
export function listMcpResourceTemplates(request: {
	name: string;
	targetBinding: string;
	forceRefresh: boolean;
}): Promise<McpResourceTemplateListView> {
	return invoke('list_mcp_resource_templates', request).then((value: unknown) =>
		parseMcpResourceTemplateList(value),
	);
}

/** Read only the exact opaque URI selected by the user from its live target. */
export function readMcpResource(request: {
	name: string;
	targetBinding: string;
	uri: string;
	forceRefresh: boolean;
}): Promise<McpFeatureCallOutcomeView> {
	return invoke('read_mcp_resource', request).then((value: unknown) =>
		parseMcpFeatureCallOutcome(value),
	);
}

/** Watch updates for one resource explicitly selected from the current server list. */
export function subscribeMcpResource(request: {
	name: string;
	targetBinding: string;
	uri: string;
}): Promise<boolean> {
	return invoke('subscribe_mcp_resource', request).then((value: unknown) => {
		if (typeof value !== 'boolean')
			throw new Error('Invalid MCP resource subscription response');
		return value;
	});
}

/** Stop watching one resource on the selected live connection. */
export function unsubscribeMcpResource(request: {
	name: string;
	targetBinding: string;
	uri: string;
}): Promise<boolean> {
	return invoke('unsubscribe_mcp_resource', request).then((value: unknown) => {
		if (typeof value !== 'boolean')
			throw new Error('Invalid MCP resource unsubscribe response');
		return value;
	});
}

/** Continue a selected resource read or prompt after explicit user input. */
export function respondMcpFeatureInput(request: {
	name: string;
	targetBinding: string;
	requestId: number;
	answers: Record<string, McpElicitationAnswerView>;
}): Promise<McpFeatureCallOutcomeView> {
	return invoke('respond_mcp_feature_input', request).then((value: unknown) =>
		parseMcpFeatureCallOutcome(value),
	);
}

/** Cancel one pending interaction owned by the current MCP feature dialog. */
export function cancelMcpFeatureInput(request: {
	name: string;
	targetBinding: string;
	requestId: number;
}): Promise<boolean> {
	return invoke('cancel_mcp_feature_input', request).then((value: unknown) =>
		parseBooleanResponse(value, 'cancel_mcp_feature_input'),
	);
}

/** Discover prompts for explicit user selection. */
export function listMcpPrompts(request: {
	name: string;
	targetBinding: string;
	forceRefresh: boolean;
}): Promise<McpPromptListView> {
	return invoke('list_mcp_prompts', request).then((value: unknown) => parseMcpPromptList(value));
}

/** Render a selected prompt with string arguments declared by that prompt. */
export function getMcpPrompt(request: {
	name: string;
	targetBinding: string;
	promptName: string;
	arguments: Record<string, string>;
}): Promise<McpFeatureCallOutcomeView> {
	return invoke('get_mcp_prompt', request).then((value: unknown) =>
		parseMcpFeatureCallOutcome(value),
	);
}

/** Read the current Skill metadata projection. */
export function listSkills(): Promise<SkillInfoView[]> {
	return invoke('list_skills').then((value: unknown) => parseSkillInfoList(value));
}

/** Clear local tool circuit state. */
export function resetToolCircuits(): Promise<void> {
	return invoke('reset_tool_circuits');
}

/** Read the bounded review projection for one exact Shell or Skill step. */
export function getSandboxOutputReview(request: {
	sessionId: string;
	sourceStepId: string;
}): Promise<SandboxOutputReviewManifestDto | null> {
	return invoke('get_sandbox_output_review', request).then((value: unknown) =>
		parseSandboxOutputReview(value),
	);
}

/** Explicitly discard the private output and baseline for one Shell or Skill step. */
export function discardSandboxOutputReview(request: {
	sessionId: string;
	sourceStepId: string;
}): Promise<boolean> {
	return invoke('discard_sandbox_output_review', request).then((value: unknown) =>
		parseBooleanResponse(value, 'discard_sandbox_output_review'),
	);
}

/** Open the native save dialog and return only its user-selected candidate path. */
export function pickSandboxOutputDestination(suggestedName: string): Promise<string | null> {
	return invoke('pick_sandbox_output_destination', { suggestedName });
}

/** Queue an exact, operation-scoped one-time authorization for one manifest entry. */
export function promoteSandboxOutput(request: {
	sessionId: string;
	sourceStepId: string;
	changeIndex: number;
	destination: string;
}): Promise<void> {
	return invoke('promote_sandbox_output', request);
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
