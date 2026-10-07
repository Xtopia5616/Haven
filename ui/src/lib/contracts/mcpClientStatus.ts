import {
	MCP_CLIENT_STATUS_UNIT_VALUES,
	type McpClientStatus,
} from './generatedCommands.ts';

type WireRecord = Record<string, unknown>;

function isRecord(value: unknown): value is WireRecord {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isOneOf<const Values extends readonly string[]>(
	value: unknown,
	values: Values,
): value is Values[number] {
	return typeof value === 'string' && values.includes(value);
}

/** Validate the generated Rust MCP status union at every renderer boundary. */
export function isMcpClientStatus(value: unknown): value is McpClientStatus {
	if (isOneOf(value, MCP_CLIENT_STATUS_UNIT_VALUES)) return true;
	if (!isRecord(value) || Object.keys(value).length !== 1 || !isRecord(value.Offline))
		return false;
	return Object.keys(value.Offline).length === 1 && typeof value.Offline.error === 'string';
}
