import { writable } from 'svelte/store';

// Per-session message storage: { [sessionId: string]: Message[] }
// Special key '_draft' holds messages that haven't been assigned to a session yet
// (e.g. transcribed text before the session is created).
export const sessionMessagesStore = writable<Record<string, any[]>>({});

export const DRAFT_KEY = '_draft';

export function setSessionMessages(sessionId: string, messages: unknown[]) {
	sessionMessagesStore.update((m) => ({ ...m, [sessionId]: messages }));
}

export function addSessionMessage(sessionId: string, msg: unknown) {
	sessionMessagesStore.update((m) => {
		const list = m[sessionId] || [];
		return { ...m, [sessionId]: [...list, msg] };
	});
}

export function updateSessionMessages(sessionId: string, fn: (list: any[]) => any[]) {
	sessionMessagesStore.update((m) => {
		const list = m[sessionId] || [];
		const nextList = fn(list);
		// Skip the write when the updater returned the same array reference
		// (a no-op): Svelte stores notify every subscriber on update, and the
		// streaming path calls this once per chunk.
		if (nextList === list) return m;
		return { ...m, [sessionId]: nextList };
	});
}

// Track per-step streaming sequence numbers to detect and reject duplicates
// from Tauri event replay after page navigation. Each entry also records the
// session the message belongs to so `clearSeqMap` can prune a whole session's
// bookkeeping (streaming message ids carry no session prefix, so a plain
// `includes` check could never match).
const seqMap = new Map();
const seqSessionOf = new Map();
/** @param {string} stepId @param {number|null|undefined} seq @param {string|null} [sessionId] */
export function seqLastSeen(
	stepId: string,
	seq: number | null | undefined,
	sessionId: string | null = null,
) {
	if (seq == null) return false;
	if (sessionId) seqSessionOf.set(stepId, sessionId);
	const last = seqMap.get(stepId) ?? -1;
	if (seq <= last) return true;
	seqMap.set(stepId, seq);
	return false;
}

/** Remove seq tracking for a completed step to keep the map bounded. */
export function pruneSeq(stepId: string) {
	seqMap.delete(stepId);
	seqSessionOf.delete(stepId);
}

/** Drop seq tracking for every streamed message of one session. */
export function clearSeqMap(sessionId: string) {
	if (!sessionId) return;
	for (const [stepId, sid] of seqSessionOf) {
		if (sid === sessionId) {
			seqMap.delete(stepId);
			seqSessionOf.delete(stepId);
		}
	}
}

export function clearSessionMessages(sessionId: string) {
	if (!sessionId) return;
	clearSeqMap(sessionId);
	sessionMessagesStore.update((m) => {
		const next = { ...m };
		delete next[sessionId];
		return next;
	});
}

/**
 * Clear every per-session message list while keeping the un-sent `_draft`
 * (transcribed/typed input that was never submitted). Used after clearing
 * history: the draft belongs to no session and must survive the wipe.
 */
export function clearAllSessionMessages() {
	sessionMessagesStore.update((m) => {
		const next: Record<string, any[]> = {};
		if (Array.isArray(m[DRAFT_KEY]) && m[DRAFT_KEY].length > 0) {
			next[DRAFT_KEY] = m[DRAFT_KEY];
		}
		return next;
	});
}

// Internal: find the index to cut at for truncate/branch. Skips user
// messages (they carry no stepNumber in the live view; the resume
// builder assigns them the FOLLOWING assistant's stepNumber — cutting
// ON a user message would drop user input from the view even though
// the backend kept it).
function cutIndexForStep(list: any[], targetStep: number) {
	return list.findIndex(
		(x) => x.stepNumber != null && x.stepNumber >= targetStep && x.role !== 'user',
	);
}

/**
 * Remove all messages at or after the given step number for a session.
 * Used by rollback: the ReAct loop will re-execute from `targetStep`, so
 * any messages belonging to that step or later are stale and must be
 * dropped from the UI. User messages (no stepNumber) that appear after
 * the first removed message are also dropped since they belong to the discarded
 * timeline.
 *
 * The cut lands on the first NON-user message at/after the target step.
 * User messages carry no stepNumber in the live view, but the resume
 * builder assigns them the stepNumber of the FOLLOWING assistant message —
 * cutting ON a user message would drop the user's input from the view even
 * though the backend kept it in the session (rollback only discards
 * messages persisted after the branch point).
 */
export function truncateSessionMessages(sessionId: string, targetStep: number) {
	if (!sessionId) return;
	sessionMessagesStore.update((m) => {
		const list = m[sessionId] as unknown[] | undefined;
		if (!list || list.length === 0) return m;
		const cutIdx = cutIndexForStep(list, targetStep);
		if (cutIdx === -1) return m;
		const next = { ...m };
		next[sessionId] = list.slice(0, cutIdx);
		return next;
	});
	// Clear all seq tracking for this session. Remaining messages (before the
	// rollback point) are already finalized, so their seq entries are stale
	// anyway. This avoids fragile key-string parsing for step numbers.
	clearSeqMap(sessionId);
}

// Move all messages from `fromKey` to `toKey` in a single store update.
// No-op when `fromKey` is missing, empty, or equal to `toKey`.
function moveMessages(m: Record<string, any[]>, fromKey: string, toKey: string) {
	if (!fromKey || !toKey || fromKey === toKey) return m;
	const list = m[fromKey];
	if (!list || list.length === 0) return m;
	const next = { ...m };
	next[fromKey] = [];
	// Migrated messages (adoptDraftMessages / moveSessionMessages) are the user
	// input that CREATED the target session, so they logically precede any agent
	// content already in `toKey`. The backend can stream the first
	// "Thinking…" reasoning block before the session:created handler migrates the
	// optimistic user bubble; appending (old behavior) then renders the user's
	// opening message AFTER the reasoning. Prepend instead so the user input
	// always leads the conversation.
	//
	// The session was created because the agent accepted this input, so the
	// migrated user message(s) are already "received": mark them so the ✓
	// shows on the very first bubble too (the `agent:supplement` event only
	// covers mid-turn steering, never the opening message).
	// Opening migrate: mark received and drop any sticky `steering` that a
	// parallel busy session's global modelState may have stamped on draft.
	next[toKey] = [
		...list.map((x) =>
			x.role === 'user' ? { ...x, received: true, steering: false } : x,
		),
		...(next[toKey] || []),
	];
	return next;
}

// Move draft messages to a real session (called when session:created fires).
// Return whether a draft was actually adopted so the session lifecycle
// handler can bind the newly-created session before a very fast model run
// reaches session:completed. Without that handoff, terminal cleanup can
// evict the only in-memory copy before process_transcript returns its result.
export function adoptDraftMessages(sessionId: string): boolean {
	let adopted = false;
	sessionMessagesStore.update((m) => {
		if (!Array.isArray(m[DRAFT_KEY]) || m[DRAFT_KEY].length === 0) return m;
		adopted = true;
		return moveMessages(m, DRAFT_KEY, sessionId);
	});
	return adopted;
}

/**
 * Move messages between session keys. Used when the backend reports
 * `SessionCreated` for a voice/typed submission whose messages were appended
 * under a different key — either `_draft` (no session was open) or a stale session
 * id the UI auto-restored while STT was running. Without the move, the user's
 * message would stay hidden in the old key while the new session only shows
 * the agent's reply.
 */
export function moveSessionMessages(fromSessionId: string, toSessionId: string) {
	sessionMessagesStore.update((m) => moveMessages(m, fromSessionId, toSessionId));
}
