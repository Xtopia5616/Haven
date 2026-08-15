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

// ── Step / tool id factories ──────────────────────────────────────────────
// Single source of truth for the streaming ids. Every event handler builds
// the same id for the same (session, step, run) triple; a mismatch would split
// one step into two message streams.

/** `thought-<sessionId>-<step>-<run>` / `reasoning-...` / `tool-...` */
export const stepId = (prefix, sessionId, stepNumber, runId) =>
	`${prefix}-${sessionId}-${stepNumber}-${runId ?? 0}`;

/** `tool-<sessionId>-<step>-<run>-<callIdOrName>` */
export const toolId = (sessionId, stepNumber, runId, callIdOrName) =>
	`${stepId('tool', sessionId, stepNumber, runId)}-${callIdOrName}`;

/**
 * Finalize every streaming block belonging to a step: the reasoning block
 * and the thought block. Shared by the silent and visible `agent:action`
 * branches. Finalized blocks drop straggler chunks that flush out of the
 * batcher after the event.
 */
export function finalizeStreamBlocks(messages, reasoningId, thoughtId) {
	return messages.map((x) =>
		(x.id === reasoningId || x.id === thoughtId || x.id.startsWith(thoughtId + '-'))
			? { ...x, streaming: false }
			: x
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
		...(isAsk && askOptions ? { options: askOptions, awaiting: true } : {}),
	};
}

export function thoughtSegmentIds(messages, baseId) {
	return messages
		.filter((x) => x.id === baseId || x.id.startsWith(baseId + '-'))
		.map((x) => x.id);
}

// Streaming blocks always live at the tail of the conversation (or just in
// front of the step's own thought message), so scanning backwards finds a
// unique step id in O(tail-distance) instead of O(whole list) — a full
// forward scan per chunk would cost O(n) on every batched flush of a long
// conversation. The id is unique, so direction never changes the result.
function lastIndexById(messages, id) {
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
	time = '',
}) {
	return {
		id,
		role: 'assistant',
		content,
		type: msgType,
		voice: false,
		stepNumber,
		time,
		streaming,
	};
}

/**
 * Fold one streamed delta into the in-memory message list for a step.
 * @param {Array<object>} messages
 * @param {{ stepId: string, stepIdPrefix: string, delta: string, msgType: string|undefined, stepNumber: number, time: string }} opts
 * @returns {Array<object>}
 */
export function accumulateStreamChunk(messages, opts) {
	const { stepId, stepIdPrefix, delta, msgType, stepNumber, time } = opts;
	if (!delta) return messages;

	// One streaming block per step (reasoning and thought alike): no sentence
	// splitting, so the bubble never fragments mid-stream.
	const idx = lastIndexById(messages, stepId);
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
		// than the accumulated text) is still rejected.
		if (messages[idx].streaming === false) {
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
	// thought message so the order is stable the whole way through.
	const thoughtBase = `thought${stepId.slice(stepIdPrefix.length)}`;
	const insertAt = messages.findIndex(
		(x) => x.id === thoughtBase || x.id.startsWith(thoughtBase + '-'),
	);
	const newMsg = newStreamMessage({ id: stepId, content: delta, msgType, stepNumber, time });
	if (insertAt < 0) return [...messages, newMsg];
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
 * @param {Array<object>} messages
 * @param {{ stepId: string, reasoningId: string, thought: string, stepNumber: number, time: string }} opts
 * @returns {Array<object>}
 */
export function applyThoughtSnap(messages, opts) {
	const { stepId, reasoningId, thought, stepNumber, time } = opts;
	const segIds = thoughtSegmentIds(messages, stepId);
	const firstSegIdx = messages.findIndex((x) => segIds.includes(x.id));
	// Reasoning may stream AFTER the thought text started (providers that
	// emit text and reasoning interleaved). The reasoning block belongs
	// above the answer, so pull it out of the list and re-insert it in
	// front of the merged thought message — otherwise the final order is
	// [answer, Thinking...] and can never self-heal.
	const reasoningRaw = messages.find((x) => x.id === reasoningId) ?? null;
	const reasoning = reasoningRaw ? { ...reasoningRaw, streaming: false } : null;
	const rest = messages.filter((x) => !segIds.includes(x.id) && x.id !== reasoningId);
	const merged = newStreamMessage({
		id: stepId,
		content: thought,
		streaming: false,
		stepNumber,
		time,
	});
	if (firstSegIdx < 0) {
		const out = [...rest];
		if (reasoning) out.push(reasoning);
		out.push(merged);
		return out;
	}
	// The merged thought replaces the first segment's slot. `rest` is the
	// original list with segments + reasoning removed, so `firstSegIdx`
	// (an index into `messages`) no longer maps directly: subtract every
	// removed item that sat before the first segment to get the equivalent
	// position in `rest`. This keeps any preceding user question and any
	// interleaved tool cards in their correct relative order — clamping
	// against `rest.length` alone mis-placed the thought/reasoning relative
	// to those messages.
	let insertAt = firstSegIdx;
	for (let i = 0; i < firstSegIdx; i++) {
		if (segIds.includes(messages[i].id) || messages[i].id === reasoningId) insertAt--;
	}
	insertAt = Math.max(0, Math.min(insertAt, rest.length));
	rest.splice(insertAt, 0, merged);
	if (reasoning) {
		// Reasoning goes immediately before the merged thought, i.e. at the
		// same insertion slot as the thought.
		rest.splice(insertAt, 0, reasoning);
	}
	return rest;
}
