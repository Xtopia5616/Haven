import { writable, type Writable } from 'svelte/store';

/**
 * Live foreground tool-output previews keyed by `step-*` id.
 * Populated by `agent:tool_output`; cleared by `agent:observation`.
 * Kept out of the message list so ticks do not rewrite the transcript store.
 */
const toolOutputPreviewSessions = new Map<string, string>();
interface ToolOutputPreviewStoreEntry {
	store: Writable<string | undefined>;
	subscriberCount: number;
	value: string | undefined;
}

const toolOutputPreviewStores = new Map<string, ToolOutputPreviewStoreEntry>();

function discardEmptyUnobservedStore(stepId: string, entry: ToolOutputPreviewStoreEntry) {
	if (entry.subscriberCount > 0 || (entry.value !== undefined && entry.value !== '')) return;
	if (toolOutputPreviewStores.get(stepId) !== entry) return;
	toolOutputPreviewStores.delete(stepId);
	toolOutputPreviewSessions.delete(stepId);
}

function createToolOutputPreviewStoreEntry(stepId: string): ToolOutputPreviewStoreEntry {
	const source = writable<string | undefined>(undefined);
	const entry: ToolOutputPreviewStoreEntry = {
		store: null as unknown as Writable<string | undefined>,
		subscriberCount: 0,
		value: undefined,
	};

	entry.store = {
		subscribe(run, invalidate) {
			entry.subscriberCount += 1;
			const unsubscribe = source.subscribe(run, invalidate);
			let subscribed = true;
			return () => {
				if (!subscribed) return;
				subscribed = false;
				unsubscribe();
				entry.subscriberCount -= 1;
				discardEmptyUnobservedStore(stepId, entry);
			};
		},
		set(value) {
			entry.value = value;
			source.set(value);
			discardEmptyUnobservedStore(stepId, entry);
		},
		update(updater) {
			entry.store.set(updater(entry.value));
		},
	};

	return entry;
}

function keyedToolOutputPreviewStore(stepId: string): Writable<string | undefined> {
	let entry = toolOutputPreviewStores.get(stepId);
	if (!entry) {
		entry = createToolOutputPreviewStoreEntry(stepId);
		toolOutputPreviewStores.set(stepId, entry);
	}
	return entry.store;
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
	toolOutputPreviewStores.get(stepId)?.store.set(undefined);
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
		toolOutputPreviewStores.get(stepId)?.store.set(undefined);
		toolOutputPreviewStores.delete(stepId);
	}
	if (!sessionId) toolOutputPreviewSessions.clear();
}
