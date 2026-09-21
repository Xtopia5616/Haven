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
import type { SessionReducer } from './sessionReducer.ts';

/** True when a send should be treated as mid-turn steering (keep agent UI above it). */
function isMidTurnSubmit(sessionId: string, reducer?: SessionReducer): boolean {
	const list = reducer
		? reducer.getMessages(sessionId)
		: get(sessionMessagesStore)[sessionId] || [];
	if (list.some((m) => m.streaming || m.steering)) return true;
	// Prior user still awaiting first agent bubble (race before modelState flips).
	for (let i = list.length - 1; i >= 0; i--) {
		const m = list[i];
		if (m.role === 'assistant' || m.type === 'tool' || m.type === 'ask') return false;
		if (m.role === 'user' && !m.received) return true;
	}
	// This session's own status — never borrow another session's busy chip.
	const st = (reducer ? reducer.getState().sessions : get(sessionStore)).find(
		(t) => t.id === sessionId,
	)?.status;
	if (isBusyStatus(st) || isPausedStatus(st)) return true;
	// Global modelState only applies to the active session.
	if ((reducer ? reducer.getState().activeSessionId : get(activeSessionIdStore)) === sessionId) {
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
 * Per-session submission coordinator. The backend no longer deduplicates
 * repeated user inputs by content (the canonical is an append-only
 * transcript), so rapid duplicate submissions — double-clicking "继续",
 * quick-reply spam — must be prevented here: an identical duplicate joins
 * the in-flight submission instead of stacking a second user message.
 *
 * A DIFFERENT submission for the same session that arrives while one is in
 * flight (a voice transcript racing a typed send, two distinct quick
 * messages) is QUEUED and delivered in order after the current one settles.
 * Independent sessions have independent lanes and may be submitted in
 * parallel — never silently dropped or globally serialized.
 * Each queued item snapshots the active session (+ fresh-start intent) at
 * enqueue time so a mid-flight session switch cannot retarget it.
 */
interface InflightSubmission {
	text: string;
	voice: boolean;
	recordingSessionId?: string;
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

type SubmissionLaneKey = string | symbol;

interface SubmissionLane {
	key: SubmissionLaneKey;
	inflight: InflightSubmission | null;
	pendingQueue: PendingSubmission[];
	/** Session id announced by a draft request before its promise settles. */
	adoptionTarget: string | null;
}

// A draft lane is intentionally shared: two sends made before the first
// session exists must append to the session created by the first send rather
// than creating two sessions. Persisted session ids use the `ses-` namespace,
// but a symbol keeps this invariant independent of id formatting.
const DRAFT_LANE_KEY = Symbol('draft-submission-lane');
const submissionLanes = new Map<SubmissionLaneKey, SubmissionLane>();

function laneFor(payload: SubmitPayload): SubmissionLane {
	if (payload.pinnedSessionId != null) {
		const draftLane = submissionLanes.get(DRAFT_LANE_KEY);
		// A draft request publishes its created id before its outer promise
		// settles. Only sends for that exact new session wait behind the draft
		// migration; an unrelated existing session remains fully independent.
		if (draftLane?.inflight && draftLane.adoptionTarget === payload.pinnedSessionId) {
			return draftLane;
		}
	}
	const key = payload.pinnedSessionId ?? DRAFT_LANE_KEY;
	let lane = submissionLanes.get(key);
	if (!lane) {
		lane = { key, inflight: null, pendingQueue: [], adoptionTarget: null };
		submissionLanes.set(key, lane);
	}
	return lane;
}

function maybeReleaseLane(lane: SubmissionLane) {
	if (
		lane.inflight == null &&
		lane.pendingQueue.length === 0 &&
		submissionLanes.get(lane.key) === lane
	) {
		submissionLanes.delete(lane.key);
	}
}

function hasAttachmentsOf(payload: SubmitOptions): boolean {
	return (
		(Array.isArray(payload.images) && payload.images.length > 0) ||
		(Array.isArray(payload.files) && payload.files.length > 0)
	);
}

function startSubmission(lane: SubmissionLane, payload: SubmitPayload) {
	const promise = doSubmit(payload)
		.then((result) => {
			if (lane.key === DRAFT_LANE_KEY && payload.pinnedSessionId == null) {
				lane.adoptionTarget = processResultSessionId(result);
			}
			return result;
		})
		.finally(() => {
			lane.inflight = null;
			drainQueue(lane);
			maybeReleaseLane(lane);
		});
	lane.inflight = {
		text: payload.text,
		voice: !!payload.voice,
		recordingSessionId: payload.recordingSessionId,
		hasAttachments: hasAttachmentsOf(payload),
		pinnedSessionId: payload.pinnedSessionId,
		freshStartAtEnqueue: payload.freshStartAtEnqueue,
		promise,
	};
	return promise;
}

function drainQueue(lane: SubmissionLane) {
	if (lane.inflight) return;
	const next = lane.pendingQueue.shift();
	if (!next) return;
	// Draft/fresh-start submissions pin `null` at enqueue. If a prior submit
	// just created/activated a session (and cleared the fresh-start intent),
	// move the whole remaining draft queue to that session so newly submitted
	// messages cannot overtake it on a newly created session lane.
	if (next.payload.pinnedSessionId == null) {
		const active = next.payload.reducer
			? next.payload.reducer.getState().activeSessionId
			: get(activeSessionIdStore);
		const intentStillFresh = get(newSessionIntentStore);
		if (active && !intentStillFresh) {
			const draftQueue = [next, ...lane.pendingQueue];
			lane.pendingQueue = [];
			const targetLanes = new Set<SubmissionLane>();
			for (const pending of draftQueue) {
				if (pending.payload.pinnedSessionId == null) {
					pending.payload = {
						...pending.payload,
						pinnedSessionId: active,
						freshStartAtEnqueue: false,
					};
				}
				const targetLane = laneFor(pending.payload);
				targetLane.pendingQueue.push(pending);
				targetLanes.add(targetLane);
			}
			maybeReleaseLane(lane);
			for (const targetLane of targetLanes) drainQueue(targetLane);
			return;
		}
	}
	startSubmission(lane, next.payload).then(next.resolve, next.reject);
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
 * @param {Array<{media_type: string, data: string, filename: string}>} [opts.files=null] - audio and ordinary file attachments
 * @param {boolean} [opts.voice=false] - true when forwarded from a voice transcript
 * @returns {Promise<any>} the `process_transcript` result
 */
interface SubmitOptions {
	images?: Array<{ media_type: string; data: string }> | null;
	files?: Array<{ media_type: string; data: string; filename: string }> | null;
	voice?: boolean;
	recordingSessionId?: string;
	/** Runtime reducer used by the chat route; omitted by legacy unit callers. */
	reducer?: SessionReducer;
}

export async function submitTranscript(
	text: string,
	{ images = null, files = null, voice = false, recordingSessionId, reducer }: SubmitOptions = {},
): Promise<any> {
	const payload: SubmitPayload = {
		text,
		images,
		files,
		voice,
		recordingSessionId,
		pinnedSessionId: reducer ? reducer.getState().activeSessionId : get(activeSessionIdStore),
		freshStartAtEnqueue: get(newSessionIntentStore),
		reducer,
	};
	const lane = laneFor(payload);
	if (lane.inflight) {
		// Identical duplicate (double-click 继续 / quick-reply spam): join the
		// in-flight submission so a second user message never stacks. Session
		// lane + fresh-start must match — the same text in another session is
		// independent and may run concurrently.
		const duplicate =
			lane.inflight.text === text &&
			lane.inflight.voice === !!voice &&
			lane.inflight.recordingSessionId === payload.recordingSessionId &&
			!lane.inflight.hasAttachments &&
			!hasAttachmentsOf(payload) &&
			lane.inflight.pinnedSessionId === payload.pinnedSessionId &&
			lane.inflight.freshStartAtEnqueue === payload.freshStartAtEnqueue;
		if (duplicate) return lane.inflight.promise;
		// A different submission for this session: queue it instead of dropping
		// it — the
		// optimistic bubble is added when it actually dispatches. Session
		// targeting was snapshotted above so a later switch cannot retarget it.
		return new Promise<any>((resolve, reject) => {
			lane.pendingQueue.push({ payload, resolve, reject });
		});
	}
	if (lane.pendingQueue.length > 0) {
		return new Promise<any>((resolve, reject) => {
			lane.pendingQueue.push({ payload, resolve, reject });
		});
	}
	return startSubmission(lane, payload);
}

async function doSubmit({
	text,
	images = null,
	files = null,
	voice = false,
	recordingSessionId,
	pinnedSessionId,
	freshStartAtEnqueue,
	reducer,
}: SubmitPayload): Promise<any> {
	const hasImages = Array.isArray(images) && images.length > 0;
	const hasFiles = Array.isArray(files) && files.length > 0;
	const hasAttachments = hasImages || hasFiles;
	// Images and files travel together as one attachment list; the backend
	// splits inline media from ordinary files at the host boundary.
	const attachments = [...(hasImages ? images : []), ...(hasFiles ? files : [])];
	// Ending a session leaves its terminal timeline visible. The fresh-start
	// intent is the boundary that routes the next message to a draft instead of
	// attempting to append to the completed session.
	const activeId = freshStartAtEnqueue ? null : pinnedSessionId;
	const sessionId = activeId || DRAFT_KEY;
	// Fresh-start intent was snapshotted when this submission was accepted
	// (enqueue or immediate start). If 新对话 is clicked while an older
	// request is in flight, that older snapshot stays false — resolving must
	// not clear the newer intent, or the blank draft would be hijacked.
	const freshStartAtDispatch = freshStartAtEnqueue;
	const steering = isMidTurnSubmit(sessionId, reducer);
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
	if (reducer) {
		reducer.dispatch({ type: 'session/messages/optimistic-added', sessionId, message: msg });
	} else {
		addSessionMessage(sessionId, msg);
	}
	// A reviewed conversation with no persisted messages yet (e.g. after
	// rolling back the very first user message) is rebuilt with a
	// display-only `placeholder-*` bubble carrying the session input text.
	// The submitted message is the real start of the conversation: drop
	// the stand-in so the original input is never shown twice.
	if (!reducer) {
		updateSessionMessages(sessionId, (list) => {
			if (!list.some((m) => m.id.startsWith('placeholder-'))) return list;
			return list.filter((m) => !m.id.startsWith('placeholder-'));
		});
	}
	try {
		const request = {
			transcript: text,
			activeSessionId: activeId || null,
			attachments: hasAttachments ? attachments : null,
			voice,
			...(recordingSessionId ? { recordingSessionId } : {}),
		};
		const result = await invoke('process_transcript', request);
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
			if (reducer) {
				reducer.dispatch({
					type: 'session/messages/accepted',
					fromSessionId: sessionId,
					toSessionId: createdId,
					optimisticId: msg.id,
					persistedId: dbMsgId,
				});
				reducer.dispatch({ type: 'session/selected', sessionId: createdId });
			} else {
				moveSessionMessages(sessionId, createdId);
				activeSessionIdStore.set(createdId);
			}
			targetSessionId = createdId;
		}
		// Align the optimistic bubble with the persisted `msg-*` id so rollback
		// / continue no longer need content+timestamp guessing.
		if (reducer && (!createdId || createdId === sessionId)) {
			reducer.dispatch({
				type: 'session/messages/accepted',
				fromSessionId: sessionId,
				toSessionId: targetSessionId,
				optimisticId: msg.id,
				persistedId: dbMsgId,
			});
		} else if (dbMsgId && dbMsgId !== msg.id) {
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
		if (reducer) {
			reducer.dispatch({ type: 'session/messages/rejected', sessionId, messageId: msg.id });
		} else {
			updateSessionMessages(sessionId, (list) => list.filter((x) => x.id !== msg.id));
		}
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
