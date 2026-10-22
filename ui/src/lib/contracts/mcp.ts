/** MCP command response contracts at the Rust-to-renderer boundary. */
import { isValidMcpServerName } from '../mcpServerName.ts';
import { isMcpClientStatus } from './mcpClientStatus.ts';
import { isRecord } from './objectGuards.ts';
import {
	isBoolean,
	isNonEmptyString,
	isOneOf,
	isSafeInteger,
	isString,
	isStringArray,
} from './valueGuards.ts';
import {
	MCP_SERVER_TRANSPORT_VALUES,
	MCP_TOOL_EXPOSURE_VALUES,
	type McpExposureConfig as GeneratedMcpExposureConfig,
	type McpRefreshResult as GeneratedMcpRefreshResult,
	type McpServerSnapshot as GeneratedMcpServerSnapshot,
	type McpToolExposure as GeneratedMcpToolExposure,
	type McpToolInfo as GeneratedMcpToolInfo,
} from './generatedCommands.ts';

export interface McpToolInfoView {
	name: string;
	description: string;
	inputSchema: Record<string, unknown>;
}

export type McpToolExposureMode = GeneratedMcpToolExposure;
export type McpExposureConfigView = GeneratedMcpExposureConfig;

/** Renderer-facing camelCase projection of the generated Rust snapshot DTO. */
export interface McpServerSnapshotView {
	name: string;
	transport: GeneratedMcpServerSnapshot['transport'];
	command: string;
	args: string[];
	env: string[];
	cwd: string | null;
	url: string;
	enabled: boolean;
	targetBinding: string | null;
	supportsResources: boolean;
	supportsResourceSubscriptions: boolean;
	supportsPrompts: boolean;
	exposure: McpExposureConfigView;
	status: GeneratedMcpServerSnapshot['status'];
	tools: McpToolInfoView[];
	lastError: string | null;
	diagnostic: string | null;
	lastSeenAt: number | null;
}

export interface McpRefreshResultView {
	added: string[];
	removed: string[];
	updated: string[];
	failed: string[];
}

interface ValidatedMcpToolInfo extends GeneratedMcpToolInfo {
	input_schema: Record<string, unknown>;
}

interface ValidatedMcpServerSnapshot extends Omit<GeneratedMcpServerSnapshot, 'tools'> {
	tools: ValidatedMcpToolInfo[];
	exposure: McpExposureConfigView;
}

function isNullableString(value: unknown): value is string | null {
	return value === null || isString(value);
}

function isNullableMcpTargetBinding(value: unknown): value is string | null {
	return (
		value === null || (isString(value) && /^mcp-target:[0-9a-f]{16}:[0-9a-f]{16}$/u.test(value))
	);
}

function isMcpExposureMode(value: unknown): value is McpToolExposureMode {
	return isOneOf(value, MCP_TOOL_EXPOSURE_VALUES);
}

function isMcpExposureConfig(value: unknown): value is McpExposureConfigView {
	if (!isRecord(value) || !isMcpExposureMode(value.default) || !isRecord(value.tools))
		return false;
	const entries = Object.entries(value.tools);
	return (
		entries.length <= 256 &&
		entries.every(
			([pattern, exposure]) =>
				pattern.length > 0 &&
				pattern.length <= 256 &&
				![...pattern].some((character) => /[\u0000-\u001f\u007f]/u.test(character)) &&
				isMcpExposureMode(exposure),
		)
	);
}

function isMcpToolInfo(value: unknown): value is ValidatedMcpToolInfo {
	return (
		isRecord(value) &&
		isNonEmptyString(value.name) &&
		value.name.trim().length > 0 &&
		isString(value.description) &&
		isRecord(value.input_schema)
	);
}

function isMcpServerSnapshot(value: unknown): value is ValidatedMcpServerSnapshot {
	return (
		isRecord(value) &&
		isString(value.name) &&
		isValidMcpServerName(value.name) &&
		isOneOf(value.transport, MCP_SERVER_TRANSPORT_VALUES) &&
		isString(value.command) &&
		isStringArray(value.args) &&
		isStringArray(value.env) &&
		isNullableString(value.cwd) &&
		isString(value.url) &&
		isBoolean(value.enabled) &&
		isNullableMcpTargetBinding(value.target_binding) &&
		isBoolean(value.supports_resources) &&
		isBoolean(value.supports_resource_subscriptions) &&
		isBoolean(value.supports_prompts) &&
		isMcpExposureConfig(value.exposure) &&
		isMcpClientStatus(value.status) &&
		Array.isArray(value.tools) &&
		value.tools.every(isMcpToolInfo) &&
		new Set(value.tools.map((tool) => tool.name)).size === value.tools.length &&
		isNullableString(value.last_error) &&
		isNullableString(value.diagnostic) &&
		(value.last_seen_at === null || isSafeInteger(value.last_seen_at))
	);
}

function toMcpServerSnapshotView(snapshot: ValidatedMcpServerSnapshot): McpServerSnapshotView {
	return {
		name: snapshot.name,
		transport: snapshot.transport,
		command: snapshot.command,
		args: snapshot.args,
		env: snapshot.env,
		cwd: snapshot.cwd,
		url: snapshot.url,
		enabled: snapshot.enabled,
		targetBinding: snapshot.target_binding,
		supportsResources: snapshot.supports_resources,
		supportsResourceSubscriptions: snapshot.supports_resource_subscriptions,
		supportsPrompts: snapshot.supports_prompts,
		exposure: snapshot.exposure,
		status: snapshot.status,
		tools: snapshot.tools.map((tool) => ({
			name: tool.name,
			description: tool.description,
			inputSchema: tool.input_schema,
		})),
		lastError: snapshot.last_error,
		diagnostic: snapshot.diagnostic,
		lastSeenAt: snapshot.last_seen_at,
	};
}

/** Validate complete MCP rows and map Rust wire fields to renderer camelCase. */
export function parseMcpServerSnapshots(value: unknown): McpServerSnapshotView[] {
	if (!Array.isArray(value)) throw new Error('Invalid MCP server list response');

	const names = new Set<string>();
	return value.map((snapshot: unknown) => {
		if (!isMcpServerSnapshot(snapshot)) throw new Error('Invalid MCP server snapshot');
		if (names.has(snapshot.name)) throw new Error('Duplicate MCP server name');
		names.add(snapshot.name);
		return toMcpServerSnapshotView(snapshot);
	});
}

function isUniqueMcpServerNameList(value: unknown): value is string[] {
	if (
		!Array.isArray(value) ||
		!value.every((name) => isString(name) && isValidMcpServerName(name))
	) {
		return false;
	}
	return new Set(value).size === value.length;
}

function isMcpRefreshResult(value: unknown): value is GeneratedMcpRefreshResult {
	return (
		isRecord(value) &&
		isUniqueMcpServerNameList(value.added) &&
		isUniqueMcpServerNameList(value.removed) &&
		isUniqueMcpServerNameList(value.updated) &&
		isUniqueMcpServerNameList(value.failed)
	);
}

/** Validate profile identities in every category of the batch refresh result. */
export function parseMcpRefreshResult(value: unknown): McpRefreshResultView {
	if (!isMcpRefreshResult(value)) {
		throw new Error('Invalid MCP refresh response');
	}

	return {
		added: value.added,
		removed: value.removed,
		updated: value.updated,
		failed: value.failed,
	};
}
