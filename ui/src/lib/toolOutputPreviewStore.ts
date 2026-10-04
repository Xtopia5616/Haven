import { writable, type Writable } from 'svelte/store';

/**
 * Live foreground tool-output previews keyed by `step-*` id.
 * Populated by `agent:tool_output`; cleared by `agent:observation`.
 * Kept out of the message list so ticks do not rewrite the transcript store.
 */
const toolOutputPreviewSessions = new Map<string, string>();
const toolOutputPreviewStores = new Map<string, Writable<string | undefined>>();

function keyedToolOutputPreviewStore(stepId: string): Writable<string | undefined> {
	let store = toolOutputPreviewStores.get(stepId);
	if (!store) {
		store = writable<string | undefined>(undefined);
		toolOutputPreviewStores.set(stepId, store);
	}
	return store;
}

export function getToolOutputPreviewStore(stepId: string): Writable<string | undefined> {
	return keyedToolOutputPreviewStore(stepId);
}

export function setToolOutputPreview(stepId: string, output: string, sessionId?: string) {
	if (!stepId) return;
	if (sessionId) toolOutputPreviewSessions.set(stepId, sessionId);
	keyedToolOutputPreviewStore(stepId).set(output);
}

export function clearToolOutputPreview(stepId: string) {
	if (!stepId) return;
	toolOutputPreviewSessions.delete(stepId);
	toolOutputPreviewStores.get(stepId)?.set(undefined);
	toolOutputPreviewStores.delete(stepId);
}

export function clearToolOutputPreviewsForSession(sessionId: string | null) {
	const stepIds = sessionId
		? [...toolOutputPreviewSessions.entries()]
				.filter(([, owner]) => owner === sessionId)
				.map(([stepId]) => stepId)
		: [...toolOutputPreviewStores.keys()];
	if (sessionId && stepIds.length === 0) return;
	for (const stepId of stepIds) {
		toolOutputPreviewSessions.delete(stepId);
		toolOutputPreviewStores.get(stepId)?.set(undefined);
		toolOutputPreviewStores.delete(stepId);
	}
	if (!sessionId) toolOutputPreviewSessions.clear();
}
