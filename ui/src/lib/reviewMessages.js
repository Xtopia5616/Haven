// Shared conversion of a session review payload (session + session messages +
// steps) into the chat bubble message list used by the chat page and the
// history review flow.

import { formatMessageTime } from '$lib/stores.js';

/**
 * Merge DB-loaded messages with any in-memory streaming messages that
 * arrived concurrently (e.g. a session still running while the user
 * navigates back). Streams append only when the DB doesn't already have
 * a bubble with the same id.
 *
 * @param {Array<object>} dbMessages   buildReviewMessages() result
 * @param {Array<object>} existing     current sessionMessages entry
 * @param {{ dropToolSteps?: boolean }} [opts]
 *   dropToolSteps — when true, drop DB tool-step badges whose stepNumber
 *     is already represented by a live streaming tool card in `existing`.
 *     Used by switchToSession to avoid duplicate display while a session runs.
 */
export function mergeLiveStreaming(dbMessages, existing, opts = {}) {
	const { dropToolSteps = false } = opts;
	let filteredDb = dbMessages;
	if (dropToolSteps) {
		const toolSteps = new Set(
			existing.filter((m) => m.type === 'tool' && m.stepNumber != null)
				.map((m) => m.stepNumber)
		);
		filteredDb = dbMessages.filter(
			(m) => !(m.type === 'tool' && m.stepNumber != null && toolSteps.has(m.stepNumber))
		);
	}
	const dbIds = new Set(filteredDb.map((m) => m.id));
	// Reasoning: DB-persisted reasoning (id `msg.*`) and live streaming
	// reasoning (id `reasoning-*`) carry DIFFERENT ids for the same step,
	// so plain id dedup leaves two "Thinking…" bubbles after a session switch
	// mid-step. Dedup by content: the live block is kept only while its
	// text is not already persisted in the DB (streaming or not yet
	// reconciled). Same for thought text that the snap already finalized
	// but that has not been written to the DB yet — keeping it is what
	// prevents the "finalized-but-unpersisted" message from vanishing.
	const liveReasoning = existing.filter((m) => m.type === 'reasoning');
	const dbReasoningContents = new Set(
		filteredDb.filter((m) => m.type === 'reasoning').map((m) => m.content)
	);
	const liveThought = existing.filter(
		(m) => m.role === 'assistant' && m.id.startsWith('thought-')
	);
	const dbThoughtContents = new Set(
		filteredDb.filter((m) => m.role === 'assistant').map((m) => m.content)
	);
	const streaming = existing.filter((m) => {
		if (dbIds.has(m.id)) return false;
		// Still streaming: always keep, whatever it is.
		if (m.streaming) return true;
		// Finalized live reasoning: drop when the DB already persisted the
		// same text (the DB copy is authoritative).
		if (m.type === 'reasoning') {
			return m.content !== '' && !dbReasoningContents.has(m.content);
		}
		// Finalized live thought: keep only when the DB has no equivalent
		// assistant message (prevents duplicates AND drops — the snap may
		// have arrived before the DB write).
		if (m.id.startsWith('thought-')) {
			return m.content !== '' && !dbThoughtContents.has(m.content);
		}
		// Anything else finalized (user bubbles, tool cards) is represented
		// by the DB copy; drop the live version.
		return false;
	});
	// Ask cards: a session paused on an `ask` question loses its options and
	// awaiting state when rebuilt from the DB (review builds `options: []`,
	// `awaiting: false`). If a live ask card is still awaiting, prefer it
	// over the DB card so the user can answer from the quick-reply buttons.
	const liveAskAwaiting = existing.find((m) => m.type === 'ask' && m.awaiting);
	if (liveAskAwaiting) {
		filteredDb = filteredDb.filter((m) => !(m.type === 'ask'));
		const others = streaming.filter((m) => m.id !== liveAskAwaiting.id);
		return [...filteredDb, liveAskAwaiting, ...others];
	}
	return [...filteredDb, ...streaming];
}

export function buildReviewMessages(data) {
	const items = [];
	const msgs = data.messages || [];
	const session = data.session || {};

	const msgIds = new Set();
	for (const msg of msgs) {
		msgIds.add(msg.id);
		items.push({
			id: msg.id,
			role: msg.role,
			content: msg.content,
			type: msg.message_type === 'text' ? undefined : msg.message_type || undefined,
			voice: !!msg.voice,
			time: formatMessageTime(msg.created_at),
			_ts: Date.parse(msg.created_at) || 0,
			streaming: false,
			attachments: msg.attachments || [],
		});
	}

	// Steps only supplement action/observation (tool) badges.
	// Thought-only steps are skipped because their text is already
	// represented in session messages, avoiding duplication.
	for (const step of data.steps || []) {
		const stepId = `step-${step.id}`;
		if (msgIds.has(stepId)) continue;
		if (!step.action_tool) continue;
		// Silent tool steps (input `"silent": true`) are hidden in the live
		// chat; keep them hidden here so review matches the live view.
		if (step.silent) continue;
		const obs = (step.observation && step.observation !== '{}') ? step.observation : null;
		// The `ask` tool surfaces the question as a dedicated question card.
		// Its question text is ALSO persisted as an assistant session message,
		// so mark that message as the card (awaiting=false, no buttons after
		// reload) and skip a separate session-message bubble to remove the
		// duplicate display.
		if (step.action_tool === 'ask') {
			// The persisted step observation is the raw tool JSON
			// ({"ask":true,"question":...,"hint":...}), not the readable
			// question. Extract the question so it can be matched against the
			// session message and rendered as an ask card instead of a raw
			// JSON tool badge. Older records may store the question directly.
			let askText = obs;
			let askOptions = [];
			if (obs) {
				try {
					const parsed = JSON.parse(obs);
					if (parsed && typeof parsed.question === 'string') {
						askText = parsed.question;
					}
					if (Array.isArray(parsed.options)) {
						askOptions = parsed.options.map((o) =>
							typeof o === 'string' ? o : String(o?.answer ?? o ?? '')
						);
					}
				} catch {
					// Not JSON — keep the raw text as-is.
				}
			}
			// The session pauses to wait for the user's answer, so a paused
			// session's ask card is still awaiting a reply. Without this, a
			// session switch / reload renders the card without quick-reply
			// buttons and the user cannot answer from the chat view.
			const sessionPaused = data.session?.status === 'Paused';
			// When the model batches multiple ask calls into one step, the
			// persisted assistant message joins the questions with "\n\n",
			// while each step observes only its own question. Match either the
			// exact question or the joined text, and skip adding a separate
			// raw tool badge for any matched question so the batch renders as
			// a single ask card, not a card + duplicate badge.
			let deduped = false;
			if (askText) {
				for (let i = 0; i < items.length; i++) {
					const item = items[i];
					if (item.role !== 'assistant') continue;
					const isAskMatch =
						item.content === askText ||
						(item.type == null && item.content.startsWith(`${askText}\n\n`)) ||
						(item.type === 'ask' && item.content.includes(askText));
					if (isAskMatch) {
						if (item.type == null) {
							items[i] = {
								...item,
								type: 'ask',
								toolName: 'ask',
								options: askOptions,
								awaiting: sessionPaused,
							};
						}
						deduped = true;
						break;
					}
				}
			}
			if (deduped) continue;
			// No matching session message (e.g. an old session without the
			// persisted question): still surface the extracted question as an
			// ask card instead of a raw JSON tool badge.
		items.push({
			id: stepId,
			role: 'assistant',
			content: askText || '',
			type: 'ask',
			toolName: 'ask',
			options: askOptions,
			awaiting: sessionPaused,
			voice: false,
			time: formatMessageTime(step.created_at),
			_ts: Date.parse(step.created_at) || 0,
			streaming: false,
		stepNumber: step.step_number,
	});
		continue;
	}
	items.push({
		id: stepId,
		role: 'assistant',
		content: obs || '',
		type: 'tool',
		toolName: step.action_tool,
		voice: false,
		time: formatMessageTime(step.created_at),
		_ts: Date.parse(step.created_at) || 0,
		streaming: false,
		stepNumber: step.step_number,
	});
}
	// Thought-only steps are not added as separate items (their text is in
	// session messages), but we still need their step_number for stepNumber
	// inference — otherwise sessions with no tool steps (e.g. errored on the
	// first LLM call) leave all messages without a stepNumber, breaking
	// rollback. Match by content (trimmed) to the corresponding session
	// message. User items are matched too: interjections (steering) and
	// answers to paused sessions (supplements) are persisted as thought steps
	// carrying the user's own words, so the input must resolve to that step
	// even when nothing follows it (e.g. the session errored right after).
	for (const step of data.steps || []) {
		if (step.action_tool) continue;
		if (step.thought == null) continue;
		const thoughtTrimmed = step.thought.trim();
		if (!thoughtTrimmed) continue;
		for (const item of items) {
			if ((item.role === 'assistant' || item.role === 'user') && item.stepNumber == null
				&& item.content.trim() === thoughtTrimmed) {
				item.stepNumber = step.step_number;
				break;
			}
		}
	}
	items.sort((a, b) => (a._ts || 0) - (b._ts || 0));
	// Fallback: if no messages or steps exist, show the session input text
	// so the review page is not completely empty.
	if (items.length === 0 && session.input_text) {
		items.push({
			id: `placeholder-${session.id}`,
			role: 'user',
			content: session.input_text,
			voice: false,
			time: formatMessageTime(session.created_at || new Date().toISOString()),
			_ts: Date.parse(session.created_at || new Date().toISOString()) || 0,
			streaming: false,
		});
	}
	// Infer stepNumber for assistant session messages by backward-forward pass.
	// Backward: tool messages precede their action/observation in time, so
	// walking backwards assigns stepNumber to preceding assistant messages.
	let lastStep = null;
	for (let i = items.length - 1; i >= 0; i--) {
		if (items[i].stepNumber != null) lastStep = items[i].stepNumber;
		else if (items[i].role === 'assistant' && lastStep != null) items[i].stepNumber = lastStep;
	}
	// Forward: catch any assistant messages after the last tool message.
	lastStep = null;
	for (let i = 0; i < items.length; i++) {
		if (items[i].stepNumber != null) lastStep = items[i].stepNumber;
		else if (items[i].role === 'assistant' && lastStep != null) items[i].stepNumber = lastStep;
	}
	// Forward: assign stepNumber of the following assistant response to user messages.
	let nextStep = null;
	for (let i = items.length - 1; i >= 0; i--) {
		if (items[i].stepNumber != null) nextStep = items[i].stepNumber;
		else if (items[i].role === 'user' && nextStep != null) items[i].stepNumber = nextStep;
	}
	return items;
}
