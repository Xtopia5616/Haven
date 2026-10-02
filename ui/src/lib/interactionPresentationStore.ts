import { writable } from 'svelte/store';

/** Request id selected by the chat's pending-interaction dock. */
export const requestedConfirmationIdStore = writable<string | null>(null);

export function requestConfirmationOpen(id: string) {
	requestedConfirmationIdStore.set(id);
}

export function clearRequestedConfirmation() {
	requestedConfirmationIdStore.set(null);
}
