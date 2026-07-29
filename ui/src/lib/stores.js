import { writable, get } from 'svelte/store';
import { invoke } from './tauri.js';

export const recordingStore = writable({
	isRecording: false,
	isToggle: false,
	duration: 0,
});

export const taskStore = writable([]);

export const settingsStore = writable({
	llm: {
		small_model: { provider: 'openai', model: 'gpt-4o-mini', temperature: 0 },
		default_model: { provider: 'anthropic', model: 'claude-sonnet-4-20250514', temperature: 0.7 },
		balanced_model: { provider: 'local', model: 'llama3', temperature: 0.7 },
	},
	hotkey: { recording: 'Ctrl+Shift+Space', toggle: 'Ctrl+Shift+T' },
	autostart: false,
});

export const notificationStore = writable([]);

export function addNotification(msg, type = 'info', duration = 3000) {
	let id = null;
	notificationStore.update((n) => {
		if (n.some((x) => x.msg === msg && x.type === type)) {
			return n;
		}
		id = `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
		return [...n, { id, msg, type }];
	});
	if (id !== null) {
		setTimeout(() => {
			notificationStore.update((n) => n.filter((x) => x.id !== id));
		}, duration);
	}
}

// Per-task message storage: { [taskId: string]: Message[] }
// Special key '_draft' holds messages that haven't been assigned to a task yet
// (e.g. transcribed text before the task is created).
export const taskMessagesStore = writable({});

const DRAFT_KEY = '_draft';

export function setTaskMessages(taskId, messages) {
	taskMessagesStore.update((m) => ({ ...m, [taskId]: messages }));
}

export function addTaskMessage(taskId, msg) {
	taskMessagesStore.update((m) => {
		const list = m[taskId] || [];
		return { ...m, [taskId]: [...list, msg] };
	});
}

export function updateTaskMessages(taskId, fn) {
	taskMessagesStore.update((m) => ({
		...m,
		[taskId]: fn(m[taskId] || []),
	}));
}

// Track per-step streaming sequence numbers to detect and reject duplicates
// from Tauri event replay after page navigation.
const seqMap = new Map();
export function seqLastSeen(stepId, seq) {
	if (seq == null) return false;
	const last = seqMap.get(stepId) ?? -1;
	if (seq <= last) return true;
	seqMap.set(stepId, seq);
	return false;
}

/** Remove seq tracking for a completed step to keep the map bounded. */
export function pruneSeq(stepId) {
	seqMap.delete(stepId);
}

export function clearSeqMap(taskId) {
	for (const key of seqMap.keys()) {
		if (key.includes(taskId)) seqMap.delete(key);
	}
}

export function getTaskMessages(taskId) {
	const all = get(taskMessagesStore);
	return all[taskId] || [];
}

export function clearTaskMessages(taskId) {
	if (!taskId) return;
	clearSeqMap(taskId);
	taskMessagesStore.update((m) => {
		const next = { ...m };
		delete next[taskId];
		return next;
	});
}

/**
 * Remove all messages at or after the given step number for a task.
 * Used by rollback: the ReAct loop will re-execute from `targetStep`, so
 * any messages belonging to that step or later are stale and must be
 * dropped from the UI. User messages (no stepNumber) that appear after the
 * first removed message are also dropped since they belong to the discarded
 * timeline.
 */
export function truncateTaskMessages(taskId, targetStep) {
	if (!taskId) return;
	taskMessagesStore.update((m) => {
		const list = m[taskId];
		if (!list || list.length === 0) return m;
		const cutIdx = list.findIndex(
			(x) => x.stepNumber != null && x.stepNumber >= targetStep,
		);
		if (cutIdx === -1) return m;
		const next = { ...m };
		next[taskId] = list.slice(0, cutIdx);
		return next;
	});
	// Clear all seq tracking for this task. Remaining messages (before the
	// rollback point) are already finalized, so their seq entries are stale
	// anyway. This avoids fragile key-string parsing for step numbers.
	clearSeqMap(taskId);
}

// Move draft messages to a real task (called when task:created fires).
export function adoptDraftMessages(taskId) {
	taskMessagesStore.update((m) => {
		const draft = m[DRAFT_KEY] || [];
		if (draft.length === 0) return m;
		const next = { ...m };
		next[DRAFT_KEY] = [];
		next[taskId] = [...(next[taskId] || []), ...draft];
		return next;
	});
}

// Resolve visible messages for a given activeTaskId.
// Returns draft messages when no active task; otherwise returns messages
// for that task (falls back to an empty array if none stored yet).
export function resolveMessages(activeTaskId) {
	const all = get(taskMessagesStore);
	if (!activeTaskId) {
		return all[DRAFT_KEY] || [];
	}
	return all[activeTaskId] || [];
}

// Review target for navigating from history to chat with a task context.
// Set by history page before navigating to /, consumed by +page.svelte on mount.
export const reviewTargetStore = writable(null);

// Active task ID that persists across SvelteKit page navigations so the
// send handler and voice recording can supplement the same task.
export const activeTaskIdStore = writable(null);

export function newMessage({ role, content, type = null, voice = false, time }) {
	return {
		id: `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
		role,
		content,
		type,
		voice,
		time: time || new Date().toLocaleTimeString(),
	};
}

// Shared recording UI state consumed by the layout overlay.
export const recordingOverlay = writable({
	visible: false,
	isRecording: false,
	processing: false,
	sessionId: null,
	startedAt: null,
	reason: null,
	vadState: 'silent',
});

// Model state for the status chip in the titlebar.
// Driven by +page.svelte's agent:* event handlers; consumed by +layout.svelte.
export const modelStateStore = writable('ready');

let modelStateTimer = null;
export function updateModelState(state, { fallbackDelay } = /** @type {{ fallbackDelay?: number }} */ ({})) {
	if (modelStateTimer) clearTimeout(modelStateTimer);
	modelStateTimer = null;
	modelStateStore.set(state);
	if (state === 'waiting') {
		modelStateTimer = setTimeout(() => {
			modelStateStore.update((s) => (s === 'waiting' ? 'ready' : s));
		}, fallbackDelay ?? 5000);
	} else if (state === 'streaming') {
		modelStateTimer = setTimeout(() => {
			modelStateStore.update((s) => (s === 'streaming' ? 'ready' : s));
		}, fallbackDelay ?? 2000);
	}
}

export function clearModelStateTimer() {
	if (modelStateTimer) clearTimeout(modelStateTimer);
	modelStateTimer = null;
}

// Skills store for the tools page skills tab.
export const skillsStore = writable([]);

export async function refreshSkills() {
	try {
		const result = await invoke('list_skills');
		skillsStore.set(result || []);
	} catch (e) {
		console.warn('[stores] refreshSkills error:', e);
		skillsStore.set([]);
	}
}
