import { get } from 'svelte/store';
import { browser } from '$app/environment';
import {
	DRAFT_KEY,
	addSessionMessage,
	moveSessionMessages,
	sessionMessagesStore,
	updateSessionMessages,
} from './sessionMessages.ts';
import {
	activeSessionIdStore,
	modelStateStore,
	newMessage,
	newSessionIntentStore,
	NEW_ACTION_INTENT_KEY,
} from './stores.ts';
import { sessionStore } from './stores.ts';
import { isBusyStatus, isPausedStatus } from './sessionStatus.ts';
import { invoke } from './tauri.ts';

/** True when a send should be treated as mid-turn steering (keep agent UI above it). */
function isMidTurnSubmit(sessionId: string): boolean {
	const list = get(sessionMessagesStore)[sessionId] || [];
	if (list.some((m) => m.streaming || m.steering)) return true;
	// Prior user still awaiting first agent bubble (race before modelState flips).
	for (let i = list.length - 1; i >= 0; i--) {
		const m = list[i];
		if (m.role === 'assistant' || m.type === 'tool' || m.type === 'ask') return false;
		if (m.role === 'user' && !m.received) return true;
	}
	// This session's own status — never borrow another session's busy chip.
	const st = get(sessionStore).find((t) => t.id === sessionId)?.status;
	if (isBusyStatus(st) || isPausedStatus(st)) return true;
	// Global modelState only applies to the active session.
	if (get(activeSessionIdStore) === sessionId) {
		const state = get(modelStateStore);
		if (
			state === 'streaming' ||
			state === 'tool' ||
			state === 'stalled' ||
			state === 'waiting'
		) {
			return true;
		}
	}
	return false;
}

/**
 * In-flight submission lock. The backend no longer deduplicates repeated
 * user inputs by content (the canonical is an append-only transcript), so
 * rapid duplicate submissions — double-clicking "继续", quick-reply spam —
 * must be prevented here: an identical duplicate joins the in-flight
 * submission instead of stacking a second user message.
 *
 * A DIFFERENT submission that arrives while one is in flight (a voice
 * transcript racing a typed send, two distinct quick messages) is QUEUED and
 * delivered in order after the current one settles — never silently dropped.
 * Each queued item snapshots the active session (+ fresh-start intent) at
 * enqueue time so a mid-flight session switch cannot retarget it.
 */
interface InflightSubmission {
	text: string;
	voice: boolean;
	hasAttachments: boolean;
	pinnedSessionId: string | null;
	freshStartAtEnqueue: boolean;
	promise: Promise<any>;
}

interface SubmitPayload extends SubmitOptions {
	text: string;
	/** Session id (or null) captured when this submission was accepted. */
	pinnedSessionId: string | null;
	/** Fresh-start intent captured when this submission was accepted. */
	freshStartAtEnqueue: boolean;
}

/** A queued submission awaiting the in-flight one to settle. */
interface PendingSubmission {
	payload: SubmitPayload;
	resolve: (value: any) => void;
	reject: (reason: any) => void;
}

let inflight: InflightSubmission | null = null;
let pendingQueue: PendingSubmission[] = [];

function hasAttachmentsOf(payload: SubmitOptions): boolean {
	return (
		(Array.isArray(payload.images) && payload.images.length > 0) ||
		(Array.isArray(payload.files) && payload.files.length > 0)
	);
}

function startSubmission(payload: SubmitPayload) {
	const promise = doSubmit(payload).finally(() => {
		inflight = null;
		drainQueue();
	});
	inflight = {
		text: payload.text,
		voice: !!payload.voice,
		hasAttachments: hasAttachmentsOf(payload),
		pinnedSessionId: payload.pinnedSessionId,
		freshStartAtEnqueue: payload.freshStartAtEnqueue,
		promise,
	};
	return promise;
}

function drainQueue() {
	const next = pendingQueue.shift();
	if (!next) return;
	// Draft/fresh-start submissions pin `null` at enqueue. If a prior submit
	// just created/activated a session (and cleared the fresh-start intent),
	// adopt it so typed+voice (or two draft sends) append to the same
	// conversation instead of spawning a second one.
	if (next.payload.pinnedSessionId == null) {
		const active = get(activeSessionIdStore);
		const intentStillFresh = get(newSessionIntentStore);
		if (active && !intentStillFresh) {
			next.payload = {
				...next.payload,
				pinnedSessionId: active,
				freshStartAtEnqueue: false,
			};
		}
	}
	startSubmission(next.payload).then(next.resolve, next.reject);
}

/**
 * Deliver a user submission (typed input or voice transcript) to the
 * backend through the same `process_transcript` path so voice input
 * continues the currently open conversation instead of starting a new one.
 *
 * The optimistic message is appended first under the active session id, or
 * under `_draft` when no session is open. If the backend replies with
 * `SessionCreated`, the message is migrated out of wherever it landed —
 * `_draft`, or a stale session id the UI auto-restored while the request was
 * in flight (meaningful for voice; harmless for typed) — into the fresh
 * session, and the active session id is updated.
 *
 * On submission failure the optimistic bubble is removed from the same
 * key it landed in and the error is rethrown so the caller can surface
 * it (notification toast, etc.).
 *
 * @param {string} text
 * @param {object} [opts]
 * @param {Array<{media_type: string, data: string}>} [opts.images=null] - image attachments; null/empty for voice
 * @param {Array<{media_type: string, data: string, filename: string}>} [opts.files=null] - non-image file attachments
 * @param {boolean} [opts.voice=false] - true when forwarded from a voice transcript
 * @returns {Promise<any>} the `process_transcript` result
 */
interface SubmitOptions {
	images?: Array<{ media_type: string; data: string }> | null;
	files?: Array<{ media_type: string; data: string; filename: string }> | null;
	voice?: boolean;
}

export async function submitTranscript(
	text: string,
	{ images = null, files = null, voice = false }: SubmitOptions = {},
): Promise<any> {
	const payload: SubmitPayload = {
		text,
		images,
		files,
		voice,
		pinnedSessionId: get(activeSessionIdStore),
		freshStartAtEnqueue: get(newSessionIntentStore),
	};
	if (inflight) {
		// Identical duplicate (double-click 继续 / quick-reply spam): join the
		// in-flight submission so a second user message never stacks. Session
		// pin + fresh-start must match — same text after a switch is NOT a
		// duplicate and must be queued for the new target.
		const duplicate =
			inflight.text === text &&
			inflight.voice === !!voice &&
			!inflight.hasAttachments &&
			!hasAttachmentsOf(payload) &&
			inflight.pinnedSessionId === payload.pinnedSessionId &&
			inflight.freshStartAtEnqueue === payload.freshStartAtEnqueue;
		if (duplicate) return inflight.promise;
		// A different submission: queue it instead of dropping it — the
		// optimistic bubble is added when it actually dispatches. Session
		// targeting was snapshotted above so a later switch cannot retarget it.
		return new Promise<any>((resolve, reject) => {
			pendingQueue.push({ payload, resolve, reject });
		});
	}
	startSubmission(payload);
	return inflight!.promise;
}

async function doSubmit({
	text,
	images = null,
	files = null,
	voice = false,
	pinnedSessionId,
	freshStartAtEnqueue,
}: SubmitPayload): Promise<any> {
	const hasImages = Array.isArray(images) && images.length > 0;
	const hasFiles = Array.isArray(files) && files.length > 0;
	const hasAttachments = hasImages || hasFiles;
	// Images and files travel together as one attachment list; the backend
	// splits them again (images go to the vision model, files to disk).
	const attachments = [
		...(hasImages ? images : []),
		...(hasFiles ? files : []),
	];
	const activeId = pinnedSessionId;
	const sessionId = activeId || DRAFT_KEY;
	// Fresh-start intent was snapshotted when this submission was accepted
	// (enqueue or immediate start). If 新对话 is clicked while an older
	// request is in flight, that older snapshot stays false — resolving must
	// not clear the newer intent, or the blank draft would be hijacked.
	const freshStartAtDispatch = freshStartAtEnqueue;
	const steering = isMidTurnSubmit(sessionId);
	const msg = {
		...newMessage({
			role: 'user',
			content: text,
			voice,
			time: new Date().toLocaleTimeString(),
			...(hasAttachments ? { attachments, idPrefix: 'u' } : {}),
		}),
		...(steering ? { steering: true } : {}),
	};
	addSessionMessage(sessionId, msg);
	// A reviewed conversation with no persisted messages yet (e.g. after
	// rolling back the very first user message) is rebuilt with a
	// display-only `placeholder-*` bubble carrying the session input text.
	// The submitted message is the real start of the conversation: drop
	// the stand-in so the original input is never shown twice.
	updateSessionMessages(sessionId, (list) => {
		if (!list.some((m) => m.id.startsWith('placeholder-'))) return list;
		return list.filter((m) => !m.id.startsWith('placeholder-'));
	});
	try {
		const result = await invoke('process_transcript', {
			transcript: text,
			activeSessionId: activeId || null,
			attachments: hasAttachments ? attachments : null,
			voice,
		});
		const createdId = processResultSessionId(result);
		const dbMsgId = processResultMessageId(result);
		// Prefer the destination session after a SessionCreated migrate so the
		// id rewrite lands on the bubble that was just moved.
		let targetSessionId = sessionId;
		if (createdId && createdId !== sessionId) {
			// This submission created the new session. Clear the fresh-start intent
			// ONLY if the fresh-start was already active when this submission was
			// accepted — otherwise a 新对话 click that landed mid-flight must
			// not be cancelled, or the blank draft would be hijacked by this
			// (older) submission. Cleared BEFORE the store write so the page's
			// store→state follow effect (guarded by the intent) can adopt the
			// new session when appropriate.
			if (freshStartAtDispatch) {
				newSessionIntentStore.set(false);
				if (browser) localStorage.removeItem(NEW_ACTION_INTENT_KEY);
			}
			moveSessionMessages(sessionId, createdId);
			activeSessionIdStore.set(createdId);
			targetSessionId = createdId;
		}
		// Align the optimistic bubble with the persisted `msg-*` id so rollback
		// / continue no longer need content+timestamp guessing.
		if (dbMsgId && dbMsgId !== msg.id) {
			updateSessionMessages(targetSessionId, (list) => {
				const idx = list.findIndex((x) => x.id === msg.id);
				if (idx < 0) return list;
				const next = list.slice();
				// Keep steering so in-flight agent cards stay above this bubble
				// until agent:supplement clears it at inject time.
				next[idx] = { ...next[idx], id: dbMsgId };
				return next;
			});
		}
		return result;
	} catch (e) {
		updateSessionMessages(sessionId, (list) => list.filter((x) => x.id !== msg.id));
		throw e;
	}
}

/** `ProcessResult::SessionCreated { session_id }` (struct variant). */
export function processResultSessionId(result: any): string | null {
	const created = result?.SessionCreated;
	if (!created) return null;
	if (typeof created === 'string') return created;
	return typeof created.session_id === 'string' ? created.session_id : null;
}

/** Persisted user-message id from either ProcessResult variant. */
export function processResultMessageId(result: any): string | null {
	const fromCreated = result?.SessionCreated?.message_id;
	if (typeof fromCreated === 'string' && fromCreated) return fromCreated;
	const fromSupp = result?.Supplemented?.message_id;
	if (typeof fromSupp === 'string' && fromSupp) return fromSupp;
	return null;
}
