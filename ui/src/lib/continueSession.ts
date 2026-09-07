/**
 * Continue strategies for an interrupted session:
 *
 * 1. User message sent, agent never generated → pass the original user text.
 *    The message may or may not have persisted; the caller checks post-resync
 *    state and either relies on Pending resume or resubmits that text.
 * 2. LLM interrupted mid-generation → send "继续".
 */

export type ContinueStrategy =
	{ mode: 'resend_user'; text: string } | { mode: 'continue'; text: '继续' };

/** Message shape used by continue heuristics (extra fields allowed). */
export type ContinueMessage = {
	role?: string;
	type?: string | null;
	content?: string;
	id?: string;
	toolName?: string;
};

/**
 * The retry affordance belongs to the conversation tail, not only to the
 * session error event. A persisted user turn can be the last visible item
 * when a paused session has not started (or has not yet reported) its next
 * assistant block.
 */
export function shouldShowContinueButton(
	messages: ContinueMessage[],
	activeSessionError = false,
): boolean {
	if (activeSessionError) return true;
	const last = messages[messages.length - 1];
	return last?.role === 'user';
}

/** Assistant bubbles that mean the model started generating after the user turn. */
function isGenerationMessage(msg: ContinueMessage): boolean {
	if (msg.role !== 'assistant') return false;
	// Supplement badges are injected context markers, not model output.
	return msg.type !== 'supplement';
}

/**
 * Decide which continue payload to use from the pre-continue message list.
 * Must be called BEFORE `continue_session` truncates partial assistant output.
 */
export function pickContinueStrategy(messages: ContinueMessage[]): ContinueStrategy {
	let lastUserIdx = -1;
	for (let i = messages.length - 1; i >= 0; i--) {
		if (messages[i].role === 'user') {
			lastUserIdx = i;
			break;
		}
	}

	const afterUser = lastUserIdx >= 0 ? messages.slice(lastUserIdx + 1) : messages;
	const generationStarted = afterUser.some(isGenerationMessage);
	if (generationStarted) {
		return { mode: 'continue', text: '继续' };
	}

	if (lastUserIdx >= 0) {
		const text = (messages[lastUserIdx].content || '').trim();
		if (text) {
			return { mode: 'resend_user', text };
		}
	}

	return { mode: 'continue', text: '继续' };
}

/**
 * After `continue_session` + DB resync: if the original user turn is still the
 * trailing content (persisted), Pending resume alone retries it — resubmitting
 * would duplicate. If it is gone (send never landed / ghost cleanup), resubmit.
 */
export function shouldResubmitOriginalUser(
	syncedMessages: ContinueMessage[],
	originalText: string,
): boolean {
	const want = originalText.trim();
	if (!want) return false;

	let lastUserIdx = -1;
	for (let i = syncedMessages.length - 1; i >= 0; i--) {
		if (syncedMessages[i].role === 'user') {
			lastUserIdx = i;
			break;
		}
	}
	if (lastUserIdx < 0) return true;

	const lastUser = syncedMessages[lastUserIdx];
	if ((lastUser.content || '').trim() !== want) return true;

	const after = syncedMessages.slice(lastUserIdx + 1);
	if (after.some(isGenerationMessage)) return true;

	// Persisted rows use msg-*; optimistic-only bubbles keep a temp id and
	// must be resent so the backend actually receives the turn.
	const id = lastUser.id || '';
	return !/^msg-/.test(id);
}
