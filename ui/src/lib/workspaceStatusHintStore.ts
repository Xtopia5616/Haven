import { writable } from 'svelte/store';

const currentHint = writable<string | null>(null);

/** Read-only view used by the application shell's global status badge. */
export const workspaceStatusHintStore = { subscribe: currentHint.subscribe };

/** Publish the active Composer's contextual delivery hint for the status badge. */
export function publishWorkspaceStatusHint(hint: string | null) {
	currentHint.set(hint);
}
