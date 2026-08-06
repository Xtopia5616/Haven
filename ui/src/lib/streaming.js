// Pure helpers for live-stream accumulation of assistant thought/reasoning
// messages. Extracted from +page.svelte so the sentence-boundary split and
// full-text snap reconciliation can be unit-tested without a DOM.
//
// Thought chunks stream into per-sentence segments: when a chunk completes a
// sentence (ends with 。！？…!?; and not inside an unclosed code fence), the
// current message is finalized (streaming:false, segmented:true) and the next
// chunk opens a fresh segment (id `${baseId}-${N}`). The authoritative
// `agent:thought` snap then collapses every segment into a single message
// containing the full step text, so the final answer renders as one bubble no
// matter where the chunk boundaries happened to land.

// ── Step / tool id factories ──────────────────────────────────────────────
// Single source of truth for the streaming ids. Every event handler builds
// the same id for the same (task, step, run) triple; a mismatch would split
// one step into two message streams.

/** `thought-<taskId>-<step>-<run>` / `reasoning-...` / `tool-...` */
export const stepId = (prefix, taskId, stepNumber, runId) =>
	`${prefix}-${taskId}-${stepNumber}-${runId ?? 0}`;

/** `tool-<taskId>-<step>-<run>-<callIdOrName>` */
export const toolId = (taskId, stepNumber, runId, callIdOrName) =>
	`${stepId('tool', taskId, stepNumber, runId)}-${callIdOrName}`;

/**
 * Finalize every streaming block belonging to a step: the reasoning block,
 * the thought block, and any thought sentence-segments. Clearing `segmented`
 * drops straggler chunks that flush out of the batcher after the event.
 * Shared by the silent and visible `agent:action` branches.
 */
export function finalizeStreamBlocks(messages, reasoningId, thoughtId) {
	return messages.map((x) =>
		(x.id === reasoningId || x.id === thoughtId || x.id.startsWith(thoughtId + '-'))
			? { ...x, streaming: false, segmented: false }
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

const SENTENCE_END_RE = /[。！？…!?;；]$/;

function isSentenceEnd(delta) {
	return !!delta && SENTENCE_END_RE.test(delta);
}

function inUnclosedFence(content) {
	return ((content.match(/```/g) || []).length % 2) !== 0;
}

export function thoughtSegmentIds(messages, baseId) {
	return messages
		.filter((x) => x.id === baseId || x.id.startsWith(baseId + '-'))
		.map((x) => x.id);
}

function newStreamMessage({
	id,
	content,
	streaming = true,
	segmented = false,
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
		segmented,
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

	if (stepIdPrefix !== 'thought') {
		// Reasoning: one block per step, no sentence splitting.
		const idx = messages.findIndex((x) => x.id === stepId);
		if (idx >= 0 && messages[idx].streaming === false) return messages;
		if (idx >= 0) {
			const curr = messages[idx].content || '';
			const content = delta.startsWith(curr) ? delta : curr + delta;
			const next = [...messages];
			next[idx] = { ...next[idx], content, streaming: true };
			return next;
		}
		return [
			...messages,
			newStreamMessage({ id: stepId, content: delta, msgType, stepNumber, time }),
		];
	}

	const segIds = thoughtSegmentIds(messages, stepId);
	if (segIds.length === 0) {
		// The very first chunk may already complete a sentence — finalize it
		// immediately, otherwise it would stay open until the next boundary.
		const ended = isSentenceEnd(delta) && !inUnclosedFence(delta);
		return [
			...messages,
			newStreamMessage({
				id: stepId,
				content: delta,
				streaming: !ended,
				segmented: ended,
				msgType,
				stepNumber,
				time,
			}),
		];
	}

	const lastIdx = messages.findIndex((x) => x.id === segIds[segIds.length - 1]);
	const last = messages[lastIdx];
	const next = [...messages];

	if (last.streaming === false) {
		// Finalized by a sentence boundary (segmented) → the next chunk opens
		// a fresh segment. Finalized by the snap / tool action (segmented
		// cleared) → drop straggler chunks that flush out of the batcher late.
		if (!last.segmented) return messages;
		// Cumulative providers echo the whole text with every chunk — the
		// boundary-finalize was premature. Resume streaming in place instead
		// of duplicating the text in a new segment.
		if (last.content && delta.startsWith(last.content)) {
			next[lastIdx] = { ...last, content: delta, streaming: true, segmented: false };
			return next;
		}
		// The opening chunk of a new segment may itself complete a sentence.
		const byId = new Map(messages.map((x) => [x.id, x]));
		const fullContent =
			segIds.map((id) => byId.get(id)?.content ?? '').join('') + delta;
		const segEnded = isSentenceEnd(delta) && !inUnclosedFence(fullContent);
		next.push(
			newStreamMessage({
				id: `${stepId}-${segIds.length}`,
				content: delta,
				streaming: !segEnded,
				segmented: segEnded,
				msgType,
				stepNumber,
				time,
			}),
		);
		return next;
	}

	const curr = last.content || '';
	// Some non-OpenAI providers send cumulative text per chunk.
	const cumulative = delta.startsWith(curr);
	const content = cumulative ? delta : curr + delta;
	// Sentence boundary → finalize this segment so it is inserted immediately;
	// the next chunk opens a fresh one.
	const ended = !cumulative && isSentenceEnd(delta) && !inUnclosedFence(content);
	next[lastIdx] = { ...last, content, streaming: !ended, segmented: ended || last.segmented };
	return next;
}

/**
 * Reconcile the authoritative full step text (`agent:thought`) with the
 * streamed segments: finalize the reasoning block and collapse every
 * sentence-segment into a single message with the full thought text. The
 * merged message carries no `segmented` flag, so any straggler chunk that
 * flushes out of the batcher after the snap is dropped instead of opening a
 * fresh bubble.
 * @param {Array<object>} messages
 * @param {{ stepId: string, reasoningId: string, thought: string, stepNumber: number, time: string }} opts
 * @returns {Array<object>}
 */
export function applyThoughtSnap(messages, opts) {
	const { stepId, reasoningId, thought, stepNumber, time } = opts;
	const reasoningFixed = messages.map((x) =>
		x.id === reasoningId ? { ...x, streaming: false } : x
	);
	const segIds = thoughtSegmentIds(reasoningFixed, stepId);
	const firstSegIdx = reasoningFixed.findIndex((x) => segIds.includes(x.id));
	const rest = reasoningFixed.filter((x) => !segIds.includes(x.id));
	const merged = newStreamMessage({
		id: stepId,
		content: thought,
		streaming: false,
		stepNumber,
		time,
	});
	if (firstSegIdx < 0) return [...rest, merged];
	rest.splice(firstSegIdx, 0, merged);
	return rest;
}
