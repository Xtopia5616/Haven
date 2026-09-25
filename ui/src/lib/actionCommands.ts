import { invoke } from '$lib/tauri.ts';
import type { CancelActionRequest } from './contracts/commands.ts';
import { mapActionPayload, type ActionPayload } from './contracts/action.ts';

/**
 * Load and normalize the Rust ActionEvent rows returned by `list_actions`.
 * A non-array response keeps the action store's existing no-op behavior;
 * malformed individual rows are left as null for the store's generic warning.
 */
export async function listActionRows(): Promise<Array<ActionPayload | null> | null> {
	const rows: unknown = await invoke('list_actions');
	return Array.isArray(rows) ? rows.map(mapActionPayload) : null;
}

/** Invoke the action cancellation command through its named request/result contract. */
export function cancelActionCommand(request: CancelActionRequest): Promise<boolean> {
	return invoke('cancel_action', request);
}
