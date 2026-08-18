// Shared conversion of a session review payload (session + session messages +
// steps) into the chat bubble message list used by the chat page and the
// history review flow.

import { formatMessageTime } from '$lib/stores.ts';

// Sentinel the backend persists in `messages.tool_call_id` for assistant
// messages that carry an `ask` question text (see
// `ASK_MSG_TOOL_CALL_ID` in crates/agent/src/react.rs). The message exists
// so resume can re-seed the question into the canonical; the review builder
// skips it (the ask CARD is the step row) by this marker instead of content
// matching. Kept in sync with the Rust const — pinned by a test below.
export const ASK_MSG_TOOL_CALL_ID = '__ask__';

interface ReviewMessage {
	id: string;
	role?: string;
	content?: string;
	type?: string | null;
	voice?: boolean;
	time?: string;
	_ts?: number;
	streaming?: boolean;
	attachments?: Array<{ media_type: string; data: string }>;
	options?: string[];
	awaiting?: boolean;
	stepNumber?: number | null;
	toolName?: string;
}

interface ReviewStep {
	id: string;
	action_tool?: string | null;
	silent?: boolean;
	observation?: string | null;
	thought?: string | null;
	created_at: string;
	step_number: number;
}

interface ReviewMsg {
	id: string;
	role: string;
	content: string;
	message_type?: string | null;
	voice?: boolean;
	created_at: string;
	attachments?: unknown[];
	tool_call_id?: string | null;
}

interface ReviewData {
	session?: {
		id?: string;
		status?: string;
		input_text?: string;
		created_at?: string;
	} | null;
	messages?: ReviewMsg[];
	steps?: ReviewStep[];
}

/**
 * Merge DB-loaded messages with any in-memory streaming messages that
 * arrived concurrently (e.g. a session still running while the user
 * navigates back). Pure ID union: every live bubble carries the SAME id
 * the backend mints and persists its DB row under (`msg-*` for streamed
 * thought/reasoning, `step-*` for tool/ask cards), so identity is decided
 * by id alone and no content comparison is needed.
 *
 * Rules:
 * - id present in both: the DB copy wins, EXCEPT an awaiting live `ask`
 *   card (its options/awaiting may be fresher than the DB build).
 * - live only, still streaming: keep (the DB write lands at step end).
 * - live only, finalized: assistant message bubbles are kept — they may be
 *   snap-finalized but not yet written to the DB, and dropping them would
 *   make seen text vanish; the next merge converges once the row lands.
 *   Cards (tool/ask) and user bubbles have no unpersisted equivalent, so
 *   they drop with the DB rebuild.
 *
 * @param {Array<object>} dbMessages   buildReviewMessages() result
 * @param {Array<object>} existing     current sessionMessages entry
 */
export function mergeLiveStreaming(dbMessages: ReviewMessage[], existing: ReviewMessage[]): ReviewMessage[] {
	// Awaiting live ask cards carry quick-reply options the DB build may lack
	// (the pause status can land after the observation). Prefer EVERY
	// awaiting card over its DB copy so all questions in a batched step stay
	// answerable.
	const liveAskById = new Map(
		existing.filter((m) => m.type === 'ask' && m.awaiting).map((m) => [m.id, m]),
	);
	// A tool card still streaming has a live observation in flight; its DB
	// badge exists (the step row is created at tool start) but is EMPTY until
	// the observation lands, so the live card must win mid-tool.
	const liveStreamingToolById = new Map(
		existing.filter((m) => m.type === 'tool' && m.streaming).map((m) => [m.id, m]),
	);
	const out: ReviewMessage[] = [];
	const emitted = new Set<string>();
	for (const m of dbMessages) {
		const live = liveAskById.get(m.id) ?? liveStreamingToolById.get(m.id);
		if (live) {
			out.push(live);
			emitted.add(live.id);
			continue;
		}
		out.push(m);
		emitted.add(m.id);
	}
	for (const m of existing) {
		if (emitted.has(m.id)) continue;
		if (m.streaming) {
			out.push(m);
			continue;
		}
		if (m.type === 'tool' || m.type === 'ask') continue;
		if (m.role !== 'assistant') continue;
		out.push(m);
	}
	return out;
}

export function buildReviewMessages(data: ReviewData): ReviewMessage[] {
	const items: ReviewMessage[] = [];
	const msgs = data.messages || [];
	const session = data.session || {};

	const msgIds = new Set();
	for (const msg of msgs) {
		msgIds.add(msg.id);
		// Ask question messages carry the `__ask__` tool_call_id sentinel:
		// the question is rendered as the ask CARD (built from the step row
		// below), so the message bubble would duplicate it. Skipping by
		// marker avoids any content comparison.
		if (msg.tool_call_id === ASK_MSG_TOOL_CALL_ID) continue;
		items.push({
			id: msg.id,
			role: msg.role,
			content: msg.content,
			type: msg.message_type === 'text' ? undefined : msg.message_type || undefined,
			voice: !!msg.voice,
			time: formatMessageTime(msg.created_at),
			_ts: Date.parse(msg.created_at) || 0,
			streaming: false,
			attachments: (msg.attachments || []) as Array<{ media_type: string; data: string }>,
		});
	}

	// Steps only supplement action/observation (tool) badges.
	// Thought-only steps are skipped because their text is already
	// represented in session messages, avoiding duplication.
	for (const step of data.steps || []) {
		// The DB stores the step row under the very `step-*` id the backend
		// minted for the live tool card, so the badge and the live card are
		// one entity.
		const stepId = step.id;
		if (msgIds.has(stepId)) continue;
		if (!step.action_tool) continue;
		// Silent tool steps (input `"silent": true`) are hidden in the live
		// chat; keep them hidden here so review matches the live view.
		if (step.silent) continue;
		const obs = (step.observation && step.observation !== '{}') ? step.observation : null;
		// The `ask` tool surfaces the question as a dedicated question card
		// under the step row's id (matching the live card). Its question text
		// is ALSO persisted as an assistant session message for resume
		// re-seeding; new records mark that message with the `__ask__`
		// sentinel (skipped above), legacy records are matched by content.
		// Either way the message bubble is dropped so the card never
		// duplicates it.
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
			// (DB status is lowercase: "paused" covers Paused and
			// PausedAwaitingAnswer.)
			const sessionPaused = data.session?.status === 'paused';
			// Legacy-only pairing: when the model batches multiple ask calls
			// into one step, the message joins the questions with "\n\n",
			// while each step observes only its own question.
			if (askText) {
				const matchIdx = items.findIndex(
					(item) =>
						item.role === 'assistant' &&
						((item.content || '') === askText ||
							(item.type == null && (item.content || '').startsWith(`${askText}\n\n`))),
				);
				if (matchIdx >= 0) items.splice(matchIdx, 1);
			}
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
				&& (item.content || '').trim() === thoughtTrimmed) {
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
	let lastStep: number | null = null;
	for (let i = items.length - 1; i >= 0; i--) {
		if (items[i].stepNumber != null) lastStep = items[i].stepNumber as number;
		else if (items[i].role === 'assistant' && lastStep != null) items[i].stepNumber = lastStep;
	}
	// Forward: catch any assistant messages after the last tool message.
	lastStep = null;
	for (let i = 0; i < items.length; i++) {
		if (items[i].stepNumber != null) lastStep = items[i].stepNumber as number;
		else if (items[i].role === 'assistant' && lastStep != null) items[i].stepNumber = lastStep;
	}
	// Forward: assign stepNumber of the following assistant response to user messages.
	let nextStep: number | null = null;
	for (let i = items.length - 1; i >= 0; i--) {
		if (items[i].stepNumber != null) nextStep = items[i].stepNumber as number;
		else if (items[i].role === 'user' && nextStep != null) items[i].stepNumber = nextStep;
	}
	return items;
}
