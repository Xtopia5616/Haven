/**
 * Canonical public tool names and the small set of historical aliases that
 * are safe to normalize at the history/UI boundary. Runtime tool calls are
 * expected to already use the canonical spelling.
 */
const LEGACY_TOOL_NAME_ALIASES: Readonly<Record<string, string>> = Object.freeze({
	file: 'files',
	file_search: 'files',
	scheduled_action: 'schedule',
});

/**
 * Normalize a persisted tool name for renderer selection. Unknown names are
 * preserved because they may be dynamic MCP/skill tools or an intentionally
 * unrecoverable historical operation.
 */
export function canonicalToolName(name: string | null | undefined): string {
	const value = typeof name === 'string' ? name : '';
	return LEGACY_TOOL_NAME_ALIASES[value] ?? value;
}

/**
 * `process.launch` belonged to a removed operation and must not be silently
 * presented as today's `process` tool. The history builder keeps its name and
 * marks the card so the UI can explain why it cannot be replayed.
 */
export function isUnrecoverableHistoricalTool(name: string | null | undefined): boolean {
	return name === 'process.launch';
}
