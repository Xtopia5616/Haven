// Shared conversion of a task review payload (task + session messages +
// steps) into the chat bubble message list used by the chat page and the
// history review flow.

import { formatMessageTime } from '$lib/stores.js';

/**
 * Merge DB-loaded messages with any in-memory streaming messages that
 * arrived concurrently (e.g. a task still running while the user
 * navigates back). Streams append only when the DB doesn't already have
 * a bubble with the same id.
 *
 * @param {Array<object>} dbMessages   buildReviewMessages() result
 * @param {Array<object>} existing     current taskMessages entry
 * @param {{ dropToolSteps?: boolean }} [opts]
 *   dropToolSteps — when true, drop DB tool-step badges whose stepNumber
 *     is already represented by a live streaming tool card in `existing`.
 *     Used by switchToTask to avoid duplicate display while a task runs.
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
	const streaming = existing.filter((m) => m.streaming);
	return [...filteredDb, ...streaming.filter((m) => !dbIds.has(m.id))];
}

export function buildReviewMessages(data) {
	const items = [];
	const msgs = data.messages || [];
	const task = data.task || {};

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
			if (obs) {
				try {
					const parsed = JSON.parse(obs);
					if (parsed && typeof parsed.question === 'string') {
						askText = parsed.question;
					}
				} catch {
					// Not JSON — keep the raw text as-is.
				}
			}
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
								options: [],
								awaiting: false,
							};
						}
						deduped = true;
						break;
					}
				}
			}
			if (deduped) continue;
			// No matching session message (e.g. an old task without the
			// persisted question): still surface the extracted question as an
			// ask card instead of a raw JSON tool badge.
		items.push({
			id: stepId,
			role: 'assistant',
			content: askText || '',
			type: 'ask',
			toolName: 'ask',
			options: [],
			awaiting: false,
			voice: false,
			time: formatMessageTime(step.created_at),
			_ts: Date.parse(step.created_at) || 0,
			streaming: false,
			stepNumber: step.step_index,
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
		stepNumber: step.step_index,
	});
}
	// Thought-only steps are not added as separate items (their text is in
	// session messages), but we still need their step_index for stepNumber
	// inference — otherwise tasks with no tool steps (e.g. errored on the
	// first LLM call) leave all messages without a stepNumber, breaking
	// rollback. Match by content (trimmed) to the corresponding session
	// message. User items are matched too: interjections (steering) and
	// answers to paused tasks (supplements) are persisted as thought steps
	// carrying the user's own words, so the input must resolve to that step
	// even when nothing follows it (e.g. the task errored right after).
	for (const step of data.steps || []) {
		if (step.action_tool) continue;
		if (step.thought == null) continue;
		const thoughtTrimmed = step.thought.trim();
		if (!thoughtTrimmed) continue;
		for (const item of items) {
			if ((item.role === 'assistant' || item.role === 'user') && item.stepNumber == null
				&& item.content.trim() === thoughtTrimmed) {
				item.stepNumber = step.step_index;
				break;
			}
		}
	}
	items.sort((a, b) => (a._ts || 0) - (b._ts || 0));
	// Fallback: if no messages or steps exist, show the task input text
	// so the review page is not completely empty.
	if (items.length === 0 && task.input_text) {
		items.push({
			id: `placeholder-${task.id}`,
			role: 'user',
			content: task.input_text,
			voice: false,
			time: formatMessageTime(task.created_at || new Date().toISOString()),
			_ts: Date.parse(task.created_at || new Date().toISOString()) || 0,
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
