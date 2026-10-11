import { writable } from 'svelte/store';

/** Request id selected by the chat's pending-interaction dock. */
export const requestedConfirmationRequestIdStore = writable<string | null>(null);

export function requestConfirmationOpen(requestId: string) {
	requestedConfirmationRequestIdStore.set(requestId);
}

export function clearRequestedConfirmation() {
	requestedConfirmationRequestIdStore.set(null);
}
