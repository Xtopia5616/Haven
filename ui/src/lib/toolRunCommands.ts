import { invoke } from '$lib/tauri.ts';
import type { CancelToolRunRequest } from './contracts/commands.ts';
import { mapToolRunPayload, type ToolRunKind, type ToolRunPayload } from './contracts/toolRun.ts';

/**
 * Load and normalize the Rust ToolRunEvent rows returned by `list_tool_runs`.
 * A non-array response keeps the ToolRun store's existing no-op behavior;
 * malformed individual rows are left as null for the store's generic warning.
 */
export async function listToolRunRows(): Promise<Array<ToolRunPayload | null> | null> {
	const rows: unknown = await invoke('list_tool_runs');
	return Array.isArray(rows) ? rows.map(mapToolRunPayload) : null;
}

/** Load terminal ToolRun records, optionally restricted to one session. */
export async function listToolRunHistory(
	kind?: ToolRunKind,
	limit = 50,
	sessionId?: string,
): Promise<ToolRunPayload[]> {
	const rows: unknown = await invoke('list_tool_run_history', {
		kind: kind ?? null,
		limit,
		sessionId: sessionId ?? null,
	});
	return Array.isArray(rows)
		? rows.map(mapToolRunPayload).filter((row): row is ToolRunPayload => row !== null)
		: [];
}

/** Clear persisted terminal task history, preserving active work. */
export function clearToolRunHistory(): Promise<number> {
	return invoke('clear_tool_run_history');
}

/** Invoke the ToolRun cancellation command through its named request/result contract. */
export function cancelToolRunCommand(request: CancelToolRunRequest): Promise<boolean> {
	return invoke('cancel_tool_run', request);
}
