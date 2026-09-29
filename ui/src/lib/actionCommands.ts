import { invoke } from '$lib/tauri.ts';
import type { CancelActionRequest } from './contracts/commands.ts';
import { mapActionPayload, type ActionKind, type ActionPayload } from './contracts/action.ts';

/**
 * Load and normalize the Rust ActionEvent rows returned by `list_actions`.
 * A non-array response keeps the action store's existing no-op behavior;
 * malformed individual rows are left as null for the store's generic warning.
 */
export async function listActionRows(): Promise<Array<ActionPayload | null> | null> {
	const rows: unknown = await invoke('list_actions');
	return Array.isArray(rows) ? rows.map(mapActionPayload) : null;
}

/** Load terminal action records for the task history view. */
export async function listActionHistory(kind?: ActionKind, limit = 50): Promise<ActionPayload[]> {
	const rows: unknown = await invoke('list_action_history', { kind: kind ?? null, limit });
	return Array.isArray(rows)
		? rows.map(mapActionPayload).filter((row): row is ActionPayload => row !== null)
		: [];
}

/** Invoke the action cancellation command through its named request/result contract. */
export function cancelActionCommand(request: CancelActionRequest): Promise<boolean> {
	return invoke('cancel_action', request);
}
