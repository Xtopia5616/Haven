// Shared conversion of a session review payload (session + session messages +
// steps) into the chat bubble message list used by the chat page and the
// history review flow.

import { formatMessageTime } from '$lib/stores.ts';
import { isPausedStatus } from '$lib/sessionStatus.ts';

// Sentinel the backend used to persist in `messages.tool_call_id` for
// assistant messages carrying an `ask` question text. New records no
// longer set it: the question message is persisted under the ask step row's
// id, so the review builder skips it by id and renders the ask CARD from
// the message. Legacy rows still carry the sentinel — kept for them.
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
	/** Tool/ask completion time (observation landed). `null` for rows that
	 * never completed (failed mid-execution); falls back to created_at. */
	completed_at?: string | null;
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
 *   Persisted tool/ask cards (`step-*` ids) are kept the same way: Continue
 *   resync can race the retry's Action/Observation and briefly miss the
 *   pending row, and dropping them made post-resume tool calls vanish.
 *   Transient cards (e.g. `web_search`) and user bubbles still drop.
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
	const existingIdxOf = new Map(existing.map((m, i) => [m.id, i]));
	// For each emitted DB row that also exists in the live list: its position
	// in `existing` (to order finalized live-only leftovers) and its position
	// in `out` (the insertion point).
	const dbOutIdx: Array<{ existingIdx: number; outIdx: number }> = [];
	const emitted = new Set<string>();
	for (const m of dbMessages) {
		const live = liveAskById.get(m.id) ?? liveStreamingToolById.get(m.id);
		if (live) {
			out.push(live);
			emitted.add(live.id);
		} else {
			out.push(m);
			emitted.add(m.id);
		}
		const existingIdx = existingIdxOf.get(m.id);
		if (existingIdx != null) dbOutIdx.push({ existingIdx, outIdx: out.length - 1 });
	}
	// Live-only leftovers. STILL-STREAMING blocks always go last (the current
	// step's tail). Finalized assistant bubbles (snap-finalized reasoning /
	// thought whose DB write hasn't landed yet) and finalized `step-*`
	// tool/ask cards are inserted at the position of the next DB row that
	// follows them in the live list — otherwise a merge could reorder them
	// after a later message, e.g. [user, thinking, user] collapsing to
	// [user, user, thinking] for one frame.
	const streamingTail: ReviewMessage[] = [];
	const finalized: Array<{ item: ReviewMessage; existingIdx: number }> = [];
	existing.forEach((m, i) => {
		if (emitted.has(m.id)) return;
		if (m.streaming) {
			streamingTail.push(m);
			return;
		}
		const isPersistedCard =
			(m.type === 'tool' || m.type === 'ask') && String(m.id || '').startsWith('step-');
		if (m.type === 'tool' || m.type === 'ask') {
			if (!isPersistedCard) return;
		} else if (m.role !== 'assistant') {
			return;
		}
		finalized.push({ item: m, existingIdx: i });
	});
	for (const f of finalized) {
		let nextDb: { existingIdx: number; outIdx: number } | null = null;
		for (const d of dbOutIdx) {
			if (d.existingIdx > f.existingIdx && (!nextDb || d.existingIdx < nextDb.existingIdx)) {
				nextDb = d;
			}
		}
		if (nextDb) {
			out.splice(nextDb.outIdx, 0, f.item);
			for (const d of dbOutIdx) {
				if (d.outIdx >= nextDb.outIdx) d.outIdx++;
			}
		} else {
			out.push(f.item);
		}
	}
	out.push(...streamingTail);
	return out;
}

export function buildReviewMessages(data: ReviewData): ReviewMessage[] {
	const items: ReviewMessage[] = [];
	const msgs = data.messages || [];
	const session = data.session || {};

	// Message rows persisted under a step row's id (ask questions on new
	// records, thought/supplement content on new records) are the content
	// view of that step: lookups by id below replace all content matching.
	const msgById = new Map(msgs.map((m) => [m.id, m]));
	const askStepIds = new Set(
		(data.steps || []).filter((s) => s.action_tool === 'ask').map((s) => s.id),
	);

	const msgIds = new Set();
	for (const msg of msgs) {
		msgIds.add(msg.id);
		// New records persist the ask question message under the ask step
		// row's id: skip it here (the ask CARD below renders it). Legacy
		// records carry the `__ask__` sentinel — skip those too.
		if (askStepIds.has(msg.id)) continue;
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

	// Steps only supplement action/observation (tool/ask) badges.
	// Thought-only steps are skipped because their text is already
	// represented in session messages (the step row shares the message's id),
	// avoiding duplication.
	for (const step of data.steps || []) {
		// The DB stores the step row under the very `step-*` id the backend
		// minted for the live tool card, so the badge and the live card are
		// one entity. New ask records ALSO persist their question message
		// under this id — the card below renders that message, so it must
		// not be deduped away.
		const stepId = step.id;
		if (!step.action_tool) continue;
		if (msgIds.has(stepId) && step.action_tool !== 'ask') continue;
		// Silent tool steps (input `"silent": true`) are hidden in the live
		// chat; keep them hidden here so review matches the live view.
		if (step.silent) continue;
		const obs = (step.observation && step.observation !== '{}') ? step.observation : null;
		// The `ask` tool surfaces the question as a dedicated question card
		// under the step row's id (matching the live card). New records keep
		// the question text in the message row persisted under that id (the
		// single content authority; the row also re-seeds resume); legacy
		// records parse it from the observation JSON instead.
		// The card's LOGICAL position is when its content (the observation /
		// question) landed — `completed_at` — not when the tool started. The
		// step row's created_at (tool START) can fall in the same second as the
		// following message row, and the stable `_ts` sort would push the card
		// AFTER the answer it precedes. completed_at always sits between the
		// preceding thought and the next message. The displayed time keeps
		// created_at so the card matches the live view (action start).
		const cardTs = Date.parse(step.completed_at || step.created_at) || 0;
		if (step.action_tool === 'ask') {
			const askMsg = msgById.get(stepId);
			let askText = askMsg ? askMsg.content : null;
			let askOptions: string[] = [];
			if (obs) {
				try {
					const parsed: { question?: unknown; options?: unknown } = JSON.parse(obs);
					if (parsed && typeof parsed.question === 'string') {
						if (!askText) askText = parsed.question;
					} else if (!askText) {
						askText = obs;
					}
					if (Array.isArray(parsed.options)) {
						askOptions = parsed.options.map((o: unknown) =>
							typeof o === 'string' ? o : String((o as { answer?: unknown } | null)?.answer ?? o ?? '')
						);
					}
				} catch {
					// Not JSON: legacy rows store the readable question
					// directly in the observation.
					if (!askText) askText = obs;
				}
			}
			// The session pauses to wait for the user's answer, so a paused
			// session's ask card is still awaiting a reply. Without this, a
			// session switch / reload renders the card without quick-reply
			// buttons and the user cannot answer from the chat view.
			// Phase 4 / F2: `paused_awaiting_answer` is distinct from `paused`.
			const sessionPaused = isPausedStatus(data.session?.status);
			// Legacy-only pairing: when the model batches multiple ask calls
			// into one step, the message joins the questions with "\n\n",
			// while each step observes only its own question. Also covers
			// the pending-ask re-persist path (the question is re-persisted
			// as a plain assistant message). New records match by id above,
			// so this content comparison only ever fires for legacy rows.
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
				_ts: cardTs,
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
			_ts: cardTs,
			streaming: false,
			stepNumber: step.step_number,
		});
	}
	// Thought-only steps are not added as separate items (their text is in
	// session messages), but we still need their step_number for stepNumber
	// inference — otherwise sessions with no tool steps (e.g. errored on the
	// first LLM call) leave all messages without a stepNumber, breaking
	// rollback. New records share the id between the thought/supplement
	// message row and its step row, so the stepNumber resolves by id alone.
	// Legacy rows carry the thought text (user steering/supplements included,
	// whose words were stored on the thought row) — match those by trimmed
	// content so old conversations keep working.
	for (const step of data.steps || []) {
		if (step.action_tool) continue;
		const byId = items.find((i) => i.id === step.id && i.stepNumber == null);
		if (byId) {
			byId.stepNumber = step.step_number;
			continue;
		}
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
