// Pure helpers for live-stream accumulation of assistant thought/reasoning
// messages. Extracted from +page.svelte so the accumulation and full-text
// snap reconciliation can be unit-tested without a DOM.
//
// Thought and reasoning chunks stream into ONE message per step — no
// sentence splitting. Splitting produced several bubbles mid-stream that
// only merged after the authoritative `agent:thought` snap arrived, so the
// streaming view visibly disagreed with the final result. The snap still
// reconciles the step's final text, and the full-text reasoning reconcile
// repairs characters lost to batcher drops, so the last bubble always
// matches the persisted message.
//
// IDs are NOT constructed here anymore: every streaming event carries the
// `message_id` / `step_id` the backend minted (the same ids the DB rows are
// persisted under), so the live bubble, the snap and the resume copy are
// one entity and merges need no content-based dedup.

/** A chat-bubble message in the live streaming view. */
export interface StreamMessage {
	id: string;
	role?: string;
	content?: string;
	type?: string | null;
	toolName?: string;
	voice?: boolean;
	stepNumber?: number | null;
	runId?: number | null;
	time?: string;
	streaming?: boolean;
	/** Render the deterministic tool-intent label when no preamble was emitted. */
	showFallbackIntent?: boolean;
	url?: string;
	awaiting?: boolean;
	options?: string[];
	/** Mid-turn user steer/queue: keep continuing agent output above this bubble. */
	steering?: boolean;
	received?: boolean;
	_ts?: number;
}

/**
 * Index at which continuing agent output should land: after the last non-steer
 * bubble, before any trailing `steering` user messages. Blind tail-appends
 * would push tools/thoughts below an optimistic steer and jump the pending
 * user bubble around as the in-flight turn keeps producing UI.
 */
export function agentInsertIndex(messages: Array<{ role?: string; steering?: boolean }>): number {
	let i = messages.length;
	while (i > 0 && messages[i - 1].role === 'user' && messages[i - 1].steering) {
		i--;
	}
	return i;
}

/** Append (or splice) an agent bubble before trailing steering user messages. */
export function insertAgentMessage<T extends StreamMessage>(messages: T[], msg: T): T[] {
	const at = agentInsertIndex(messages);
	if (at >= messages.length) return [...messages, msg];
	const next = messages.slice();
	next.splice(at, 0, msg);
	return next;
}

/** `tool-<sessionId>-<step>-<run>-web_search[-<callId>]` (provider built-in
 *  search card; not a persisted entity, so its id never needs to match a DB
 *  row). One card per `callId` so DeepSeek's search → open_page → find_in_page
 *  sequence renders as separate steps. */
export const webSearchId = (
	sessionId: string,
	stepNumber: number,
	runId: number | null | undefined,
	callId: string | null | undefined = null,
) => `tool-${sessionId}-${stepNumber}-${runId ?? 0}-web_search${callId ? `-${callId}` : ''}`;

/** Live label for a built-in web_search card, keyed by phase + action. */
export function webSearchLabel(
	phase: string | null | undefined,
	action: string | null | undefined,
): string {
	const a = action || 'search';
	if (phase === 'completed') {
		if (a === 'open_page') return '已打开网页';
		if (a === 'find_in_page') return '已页内查找';
		return '已联网搜索';
	}
	if (a === 'open_page') return '正在打开网页…';
	if (a === 'find_in_page') return '正在页内查找…';
	if (phase === 'searching') return '正在搜索…';
	return '正在联网搜索…';
}

/** Card body for a built-in web_search event. A completed payload with
 *  citations becomes `{label, queries, results}`; a later status-only
 *  `completed` must not replace that JSON with the label. */
export function webSearchCardContent(
	data: { phase?: string | null; action?: string | null; result?: unknown },
	existingContent?: string | null,
): string {
	const label = webSearchLabel(data.phase, data.action);
	if (data.result && typeof data.result === 'object') {
		return JSON.stringify({ label, ...(data.result as Record<string, unknown>) });
	}
	if (typeof existingContent === 'string' && existingContent.trimStart().startsWith('{')) {
		return existingContent;
	}
	return label;
}

/** True when `id` is `blockId` or a post-boundary segment (`blockId-N`). */
export function isStreamSegment(id: string, blockId: string | null | undefined): boolean {
	if (!blockId) return false;
	return id === blockId || id.startsWith(blockId + '-');
}

/**
 * Finalize every streaming block belonging to a step: the reasoning block
 * and the thought block, including post-tool / post-websearch segments
 * (`id-N`). Shared by every `agent:action` branch.
 * Finalized blocks drop straggler chunks that flush out of the batcher
 * after the event.
 */
export function finalizeStreamBlocks(
	messages: StreamMessage[],
	reasoningId: string | null | undefined,
	thoughtId: string | null | undefined,
) {
	return messages.map((x) =>
		isStreamSegment(x.id, reasoningId) || isStreamSegment(x.id, thoughtId)
			? { ...x, streaming: false }
			: x,
	);
}

/**
 * Remove text that was streamed before a tool call but rejected by the backend
 * as a non-meaningful fragment. The action event carries this decision, so the
 * UI does not need to guess based on text length.
 */
export function dropStreamedThought(
	messages: StreamMessage[],
	thoughtId: string | null | undefined,
) {
	return messages.filter((x) => !isStreamSegment(x.id, thoughtId) || x.type === 'reasoning');
}

/**
 * Remove live thought/reasoning blocks before a replacement provider attempt
 * starts. Tool/search cards and user messages remain because this is an
 * output-generation boundary, not a transcript rollback.
 */
export function resetStreamBlocks(
	messages: StreamMessage[],
	reasoningId: string | null | undefined,
	thoughtId: string | null | undefined,
) {
	return messages.filter(
		(x) => !isStreamSegment(x.id, reasoningId) && !isStreamSegment(x.id, thoughtId),
	);
}

/**
 * Build a tool message. Shared by the `agent:action` placeholder (streaming
 * true, no content) and the `agent:observation` fill (content + optional ask
 * options). The `ask` tool surfaces as a dedicated question card, not a raw
 * tool badge. `time` is omitted entirely when falsy so an observation fill
 * doesn't clobber the placeholder's timestamp via spread.
 */
export function newToolMessage({
	id,
	stepNumber,
	toolName,
	time = undefined,
	content = '',
	streaming = false,
	askOptions = null,
	actionId = null,
	toolArgs = undefined,
	showFallbackIntent = undefined,
	outcome = undefined,
}: {
	id: string;
	stepNumber: number;
	toolName: string;
	time?: string | undefined;
	content?: string;
	streaming?: boolean;
	askOptions?: string[] | null;
	actionId?: string | null;
	/** Live Action.input or resume action_input; omitted on observation fills
	 * so the placeholder's args are preserved via object spread. */
	toolArgs?: unknown;
	showFallbackIntent?: boolean | undefined;
	outcome?: string | null | undefined;
}) {
	const isAsk = toolName === 'ask';
	return {
		id,
		role: 'assistant',
		content,
		toolName,
		type: isAsk ? 'ask' : 'tool',
		voice: false,
		stepNumber,
		...(time ? { time } : {}),
		streaming,
		...(showFallbackIntent !== undefined ? { showFallbackIntent } : {}),
		...(outcome ? { outcome } : {}),
		...(actionId ? { actionId } : {}),
		...(toolArgs !== undefined ? { toolArgs } : {}),
		...(isAsk && askOptions ? { options: askOptions, awaiting: true } : {}),
	};
}

/** Extract a background `action_id` from a shell/tool observation payload. */
export function actionIdFromObservation(observation: string | undefined | null): string | null {
	if (!observation) return null;
	try {
		const j = JSON.parse(observation);
		if (
			j &&
			typeof j === 'object' &&
			j.background === true &&
			typeof j.action_id === 'string'
		) {
			return j.action_id;
		}
	} catch {
		// not JSON
	}
	return null;
}

/**
 * Parse a producer-labelled `[Background action result]` inject body into a
 * compact card payload for the chat UI (auto-wake bridge). Returns null when
 * the text is not an action-result inject.
 */
export function parseActionResultInject(text: string | undefined | null): {
	action_id: string | null;
	status: string;
	operation: 'result_injected';
	auto: true;
} | null {
	if (!text || !text.startsWith('[Background action result]')) return null;
	let actionId: string | null = null;
	let status = 'completed';
	for (const line of text.split('\n')) {
		const mId = /^action_id:\s*(.+)\s*$/.exec(line);
		if (mId) actionId = mId[1].trim();
		const mSt = /^status:\s*(.+)\s*$/.exec(line);
		if (mSt) status = mSt[1].trim();
	}
	return { operation: 'result_injected', action_id: actionId, status, auto: true };
}

// Streaming blocks always live at the tail of the conversation (or just in
// front of the step's own thought message), so scanning backwards finds a
// unique message id in O(tail-distance) instead of O(whole list) — a full
// forward scan per chunk would cost O(n) on every batched flush of a long
// conversation. The id is unique, so direction never changes the result.
function lastIndexById(messages: StreamMessage[], id: string) {
	for (let i = messages.length - 1; i >= 0; i--) {
		if (messages[i].id === id) return i;
	}
	return -1;
}

function newStreamMessage({
	id,
	content,
	streaming = true,
	msgType = undefined,
	stepNumber = null,
	runId = null,
	time = '',
}: {
	id: string;
	content: string;
	streaming?: boolean;
	msgType?: string | undefined;
	stepNumber?: number | null;
	runId?: number | null;
	time?: string;
}): StreamMessage {
	return {
		id,
		role: 'assistant',
		content,
		type: msgType,
		voice: false,
		stepNumber,
		runId,
		time,
		streaming,
	};
}

/**
 * Fold one streamed delta into the in-memory message list for a step.
 * @param {Array<object>} messages
 * @param {{ messageId: string, delta: string, msgType: string|undefined, stepNumber: number, runId: number, time: string }} opts
 * @returns {Array<object>}
 */
/** Next suffix id for a post-tool / post-websearch thought segment
 *  (`messageId-1`, `messageId-2`, …). Scans existing siblings so repeated
 *  mid-stream tool boundaries keep opening fresh bubbles. */
function nextSegmentId(messages: StreamMessage[], messageId: string): string {
	const prefix = messageId + '-';
	let max = 0;
	for (const m of messages) {
		if (!m.id.startsWith(prefix)) continue;
		const n = Number(m.id.slice(prefix.length));
		if (Number.isFinite(n) && n > max) max = n;
	}
	return `${messageId}-${max + 1}`;
}

/** Append `delta` onto an already-streaming segment, or open a new one at the
 *  tail when every sibling of `messageId` has been finalized (websearch /
 *  local tool boundary). */
function appendAfterFinalized(
	messages: StreamMessage[],
	opts: {
		messageId: string;
		delta: string;
		msgType: string | undefined;
		stepNumber: number;
		runId: number;
		time: string;
	},
): StreamMessage[] {
	const { messageId, delta, msgType, stepNumber, runId, time } = opts;
	const prefix = messageId + '-';
	// Prefer the newest still-streaming sibling so consecutive deltas after a
	// single boundary stay in one bubble.
	for (let i = messages.length - 1; i >= 0; i--) {
		const id = messages[i].id;
		if ((id === messageId || id.startsWith(prefix)) && messages[i].streaming === true) {
			const curr = messages[i].content || '';
			const content = delta.startsWith(curr) ? delta : curr + delta;
			const next = [...messages];
			next[i] = { ...next[i], content, streaming: true };
			return next;
		}
	}
	const earlier = contentBeforeIndex(messages, messageId, messages.length);
	let content = delta;
	if (earlier && delta.startsWith(earlier) && delta.length >= earlier.length) {
		const remainder = delta.slice(earlier.length);
		if (!remainder) return messages;
		content = remainder;
	}
	const newMsg = newStreamMessage({
		id: nextSegmentId(messages, messageId),
		content,
		msgType,
		stepNumber,
		runId,
		time,
	});
	return insertAgentMessage(messages, newMsg);
}

/** Concatenate original + completed segments sitting before `liveIdx`. */
function contentBeforeIndex(messages: StreamMessage[], messageId: string, liveIdx: number): string {
	const prefix = messageId + '-';
	let out = '';
	for (let i = 0; i < liveIdx; i++) {
		const id = messages[i].id;
		if (id === messageId || id.startsWith(prefix)) out += messages[i].content || '';
	}
	return out;
}

export function accumulateStreamChunk(
	messages: StreamMessage[],
	opts: {
		messageId: string;
		delta: string;
		msgType: string | undefined;
		stepNumber: number;
		runId: number;
		time: string;
	},
): StreamMessage[] {
	const { messageId, delta, msgType, stepNumber, runId, time } = opts;
	if (!delta) return messages;

	// Prefer an already-open post-boundary segment (`messageId-N`) so deltas
	// after a websearch/tool card keep appending below that card instead of
	// reopening the pre-boundary bubble.
	const segPrefix = messageId + '-';
	for (let i = messages.length - 1; i >= 0; i--) {
		if (messages[i].id.startsWith(segPrefix) && messages[i].streaming === true) {
			const curr = messages[i].content || '';
			// Authoritative full-text reconcile spans pre-boundary bubbles.
			// Put the remainder on this live segment instead of concatenating
			// the whole turn onto post-search text.
			const earlier = contentBeforeIndex(messages, messageId, i);
			if (earlier && delta.startsWith(earlier) && delta.length >= earlier.length) {
				const remainder = delta.slice(earlier.length);
				const next = [...messages];
				next[i] = { ...next[i], content: remainder || curr, streaming: true };
				return next;
			}
			const content = delta.startsWith(curr) ? delta : curr + delta;
			const next = [...messages];
			next[i] = { ...next[i], content, streaming: true };
			return next;
		}
	}

	// One streaming block per step (reasoning and thought alike): no sentence
	// splitting, so the bubble never fragments mid-stream — until a tool /
	// websearch finalizes it, after which further deltas open a new segment.
	const idx = lastIndexById(messages, messageId);
	if (idx >= 0) {
		const curr = messages[idx].content || '';
		// Finalized blocks normally reject new deltas — except the backend's
		// authoritative reconciliation chunk, which carries the complete text
		// so the UI can recover characters lost to batcher drops. It arrives
		// AFTER the stream is finalized (the backend guarantees it is the
		// last reasoning event for the step), so detect it by length: a
		// dropped intermediate batch makes `curr` a prefix-mismatched
		// partial, so a longer full-text delta is accepted even without the
		// `startsWith` prefix check. A straggler incremental partial (shorter
		// than the accumulated text) is still rejected — OR, when the block
		// was finalized by a mid-stream websearch/tool boundary, an
		// incremental delta opens a NEW bubble after the tool card.
		if (messages[idx].streaming === false) {
			// Mid-stream tool / websearch boundary: a card sits after this
			// bubble, so further deltas belong in a NEW bubble below it —
			// but only while that boundary is still live. After snap, every
			// post-tool thought segment is finalized; stragglers must drop
			// (not open messageId-N+1).
			let lastToolIdx = -1;
			for (let i = idx + 1; i < messages.length; i++) {
				if (messages[i].type === 'tool' || messages[i].type === 'ask') lastToolIdx = i;
			}
			if (lastToolIdx >= 0) {
				// A normal completion ends at the function call. If its final
				// deltas arrive after agent:action, they are delayed pre-tool
				// text, not a new post-tool answer. Only built-in web_search
				// deliberately resumes the same provider response below its card.
				const boundary = messages[lastToolIdx];
				if (boundary.toolName !== 'web_search') {
					const content = delta.startsWith(curr) ? delta : curr + delta;
					if (content === curr) return messages;
					const next = [...messages];
					next[idx] = { ...next[idx], content };
					return next;
				}
				const postToolThoughts = messages
					.slice(lastToolIdx + 1)
					.filter((x) => x.id === messageId || x.id.startsWith(segPrefix));
				const livePost = postToolThoughts.some((x) => x.streaming === true);
				if (livePost || postToolThoughts.length === 0) {
					return appendAfterFinalized(messages, opts);
				}
				return messages;
			}
			if (delta.length > curr.length && delta !== curr) {
				const next = [...messages];
				next[idx] = { ...next[idx], content: delta };
				return next;
			}
			return messages;
		}
		// Some providers echo the WHOLE text with every chunk instead of
		// sending incremental deltas; comparing against the accumulated text
		// detects the echo and replaces instead of concatenating garbage.
		const content = delta.startsWith(curr) ? delta : curr + delta;
		const next = [...messages];
		next[idx] = { ...next[idx], content, streaming: true };
		return next;
	}
	// Interleaved providers may stream reasoning AFTER the thought text
	// already started (text first, thinking later). Appending at the end
	// would render Thinking... below the answer until the snap finally
	// reorders it — a visible jump. Insert in front of the same step's
	// thought message (matched by (stepNumber, runId), since message ids
	// carry no step information) so the order is stable the whole way
	// through. Thought bubbles are assistant text blocks (type undefined)
	// streamed by this step's run.
	const insertAt = messages.findIndex(
		(x) =>
			x.role === 'assistant' &&
			x.type === undefined &&
			x.stepNumber === stepNumber &&
			x.runId === runId,
	);
	const newMsg = newStreamMessage({
		id: messageId,
		content: delta,
		msgType,
		stepNumber,
		runId,
		time,
	});
	if (insertAt < 0) return insertAgentMessage(messages, newMsg);
	const next = [...messages];
	next.splice(insertAt, 0, newMsg);
	return next;
}

/**
 * Reconcile the authoritative full step text (`agent:thought`) with the
 * streamed message: finalize the reasoning block and replace the thought
 * message with the complete text. The merged message carries no streaming
 * flag, so any straggler chunk that flushes out of the batcher after the
 * snap is dropped instead of reopening the bubble.
 *
 * The snap carries the SAME minted `message_id` the chunks streamed into
 * (and the DB row is later persisted under), so the reconcile is a plain
 * id-keyed replace: no content comparison is needed, and a snap arriving
 * after the list was rebuilt from the DB (page remount / session switch /
 * event replay) simply finds the authoritative copy and leaves it as is.
 * @param {Array<object>} messages
 * @param {{ messageId: string, reasoningId?: string, thought: string, stepNumber: number, runId: number, time: string }} opts
 * @returns {Array<object>}
 */
/** Relocate a trailing reasoning bubble in front of thought only when they
 *  are adjacent (no tool / websearch card between them). Mid-stream search
 *  splits must stay in place — pulling the first Thinking block past the
 *  search card would undo the split. */
function shouldRelocateReasoning(
	messages: StreamMessage[],
	reasoningId: string | undefined,
	thoughtIdx: number,
): boolean {
	if (!reasoningId || thoughtIdx < 0) return false;
	if (messages.some((x) => x.id.startsWith(reasoningId + '-'))) return false;
	const rIdx = messages.findIndex((x) => x.id === reasoningId);
	if (rIdx < 0 || rIdx < thoughtIdx) return false;
	for (let i = thoughtIdx + 1; i < rIdx; i++) {
		if (messages[i].type === 'tool' || messages[i].type === 'ask') return false;
	}
	return true;
}

function finalizeReasoningInPlace(
	messages: StreamMessage[],
	reasoningId: string | undefined,
): StreamMessage[] {
	if (!reasoningId) return messages;
	return messages.map((x) =>
		isStreamSegment(x.id, reasoningId) ? { ...x, streaming: false } : x,
	);
}

export function applyThoughtSnap(
	messages: StreamMessage[],
	opts: {
		messageId: string;
		reasoningId?: string;
		thought: string;
		stepNumber: number;
		runId: number;
		time: string;
	},
): StreamMessage[] {
	const { messageId, reasoningId, thought, stepNumber, runId, time } = opts;
	const segPrefix = messageId + '-';
	const segmentIdxs = messages
		.map((x, i) => (x.id === messageId || x.id.startsWith(segPrefix) ? i : -1))
		.filter((i) => i >= 0);
	// Mid-stream websearch / tool boundaries leave several thought segments
	// for one minted id. Do NOT collapse them back into a single bubble —
	// that would pull post-search text above the search card again. Finalize
	// every segment in place; when the authoritative text is a strict
	// extension of the earlier segments' concatenation, put the remainder
	// on the last segment (batcher-drop recovery without undoing the split).
	if (segmentIdxs.length > 1) {
		const earlier = segmentIdxs
			.slice(0, -1)
			.map((i) => messages[i].content || '')
			.join('');
		const lastIdx = segmentIdxs[segmentIdxs.length - 1];
		let lastContent = messages[lastIdx].content || '';
		if (thought.startsWith(earlier) && thought.length >= earlier.length) {
			const remainder = thought.slice(earlier.length);
			if (
				remainder === lastContent ||
				remainder.startsWith(lastContent) ||
				lastContent.startsWith(remainder) ||
				remainder.length > lastContent.length
			) {
				lastContent = remainder;
			}
		}
		return messages.map((x, i) => {
			if (i === lastIdx) return { ...x, content: lastContent, streaming: false };
			if (isStreamSegment(x.id, messageId) || isStreamSegment(x.id, reasoningId)) {
				return { ...x, streaming: false };
			}
			return x;
		});
	}
	const firstSegIdx = messages.findIndex((x) => x.id === messageId);
	const existing = firstSegIdx >= 0 ? messages[firstSegIdx] : null;
	const merged = existing
		? { ...existing, content: thought, streaming: false }
		: newStreamMessage({
				id: messageId,
				content: thought,
				streaming: false,
				stepNumber,
				runId,
				time,
			});
	if (firstSegIdx < 0) {
		return insertAgentMessage(finalizeReasoningInPlace(messages, reasoningId), merged);
	}
	if (!shouldRelocateReasoning(messages, reasoningId, firstSegIdx)) {
		return finalizeReasoningInPlace(messages, reasoningId).map((x, i) =>
			i === firstSegIdx ? merged : x,
		);
	}
	// Reasoning streamed AFTER the thought (interleaved providers). Pull it
	// in front of the answer so the final order is not [answer, Thinking...].
	const reasoningRaw = messages.find((x) => x.id === reasoningId) ?? null;
	const reasoning = reasoningRaw ? { ...reasoningRaw, streaming: false } : null;
	const rest = messages.filter((x) => x.id !== messageId && x.id !== reasoningId);
	const insertAt = Math.max(0, Math.min(firstSegIdx, rest.length));
	rest.splice(insertAt, 0, merged);
	if (reasoning) {
		rest.splice(insertAt, 0, reasoning);
	}
	return rest;
}
