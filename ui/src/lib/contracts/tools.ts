/**
 * ToolsView command response contracts derived from generated Rust wire DTOs.
 * Tool manifest projections and runtime MCP status validation remain at the renderer boundary.
 */
import type {
	McpClientStatus as GeneratedMcpClientStatus,
	McpRefreshResult as GeneratedMcpRefreshResult,
	McpServerConfig as GeneratedMcpServerConfig,
	McpServerSnapshot as GeneratedMcpServerSnapshot,
	McpToolInfo as GeneratedMcpToolInfo,
	SkillInfo as GeneratedSkillInfo,
	ToolAvailability as GeneratedToolAvailability,
	ToolIdentity as GeneratedToolIdentity,
	ToolListResponse as GeneratedToolListResponse,
	ToolManifest as GeneratedToolManifest,
	ToolModel as GeneratedToolModel,
	ToolPolicy as GeneratedToolPolicy,
	ToolPresentation as GeneratedToolPresentation,
	ToolPrompt as GeneratedToolPrompt,
	ToolRootPresentation as GeneratedToolRootPresentation,
	TauriCommandRequest,
} from './generatedCommands.ts';

export type ToolManifestIdentityWire = GeneratedToolIdentity;
export type ToolModelWire = GeneratedToolModel;
export type ToolPolicyWire = GeneratedToolPolicy;
export type ToolPresentationWire = GeneratedToolPresentation;
export type ToolRootPresentationWire = GeneratedToolRootPresentation;
export type ToolPromptWire = GeneratedToolPrompt;
export type ToolAvailabilityWire = GeneratedToolAvailability;
export type ToolManifestWire = GeneratedToolManifest;
export type ToolListResponse = GeneratedToolListResponse;
export type SkillInfo = GeneratedSkillInfo;
export type McpClientStatus = GeneratedMcpClientStatus;
export type McpToolInfo = GeneratedMcpToolInfo;
export type McpServerSnapshot = GeneratedMcpServerSnapshot;
export type McpServerConfig = GeneratedMcpServerConfig;
export type McpServerConfigInput = TauriCommandRequest<'add_mcp_server'>['config'];
export type McpRefreshResult = GeneratedMcpRefreshResult;

/** Flat command arguments are aliases of the generated Rust handler shapes. */
export type McpNameRequest = TauriCommandRequest<'reconnect_mcp'>;
export type SetEnabledRequest = TauriCommandRequest<'set_tool_enabled'>;
export type ToggleMcpServerRequest = TauriCommandRequest<'toggle_mcp_server'>;
export type UpdateMcpServerRequest = TauriCommandRequest<'update_mcp_server'>;
