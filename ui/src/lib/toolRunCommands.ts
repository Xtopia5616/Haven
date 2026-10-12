import { invoke } from '$lib/tauri.ts';
import { parseBooleanResponse } from './contracts/boolean.ts';
import { parseCountResponse } from './contracts/count.ts';
import type { CancelToolRunRequest } from './contracts/commands.ts';
import {
	mapToolRunCommandRow,
	type ToolRunCommandRow,
	type ToolRunKind,
} from './contracts/toolRun.ts';

/**
 * Load and normalize the Rust ToolRunEvent rows returned by `list_tool_runs`.
 * A non-array response keeps the ToolRun store's existing no-op behavior;
 * malformed individual rows are left as null for the store's generic warning.
 */
export async function listToolRunRows(): Promise<Array<ToolRunCommandRow | null> | null> {
	const rows: unknown = await invoke('list_tool_runs');
	return Array.isArray(rows) ? rows.map((row) => mapToolRunCommandRow(row)) : null;
}

/** Load terminal ToolRun records, optionally restricted to one session. */
export async function listToolRunHistory(
	kind?: ToolRunKind,
	limit = 50,
	sessionId?: string,
): Promise<ToolRunCommandRow[]> {
	const rows: unknown = await invoke('list_tool_run_history', {
		kind: kind ?? null,
		limit,
		sessionId: sessionId ?? null,
	});
	return Array.isArray(rows)
		? rows
				.map((row) => mapToolRunCommandRow(row, sessionId))
				.filter((row): row is ToolRunCommandRow => row !== null)
		: [];
}

/** Clear persisted terminal task history, preserving active work. */
export function clearToolRunHistory(): Promise<number> {
	return invoke('clear_tool_run_history').then((value: unknown) =>
		parseCountResponse(value, 'clear_tool_run_history'),
	);
}

/** Invoke the ToolRun cancellation command through its named request/result contract. */
export function cancelToolRunCommand(request: CancelToolRunRequest): Promise<boolean> {
	return invoke('cancel_tool_run', request).then((value: unknown) =>
		parseBooleanResponse(value, 'cancel_tool_run'),
	);
}
