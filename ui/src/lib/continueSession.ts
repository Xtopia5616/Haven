/**
 * Continue strategies for an interrupted session:
 *
 * 1. User message sent, agent never generated → pass the original user text.
 *    The message may or may not have persisted; the caller checks post-resync
 *    state and either relies on Pending resume or resubmits that text.
 * 2. LLM interrupted mid-generation → send "继续".
 */

import type { CanonicalRole } from './contracts/generatedCommands.ts';
import type { SessionMessagePresentationType } from './streaming.ts';

export type ContinueStrategy =
	{ mode: 'resend_user'; text: string; messageId?: string } | { mode: 'continue'; text: '继续' };

/** Message shape used by continue heuristics (extra fields allowed). */
export type ContinueMessage = {
	role?: CanonicalRole;
	type?: SessionMessagePresentationType | null;
	content?: string;
	id?: string;
	toolName?: string;
};

/**
 * The retry affordance belongs to the session transcript tail, not only to the
 * session error event. A persisted user turn can be the last visible item
 * when a paused session has not started (or has not yet reported) its next
 * assistant block.
 */
export function shouldShowContinueButton(
	messages: ContinueMessage[],
	activeRunFailed = false,
): boolean {
	if (activeRunFailed) return true;
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
		const lastUser = messages[lastUserIdx];
		const text = (lastUser.content || '').trim();
		if (text) {
			return { mode: 'resend_user', text, messageId: lastUser.id };
		}
	}

	return { mode: 'continue', text: '继续' };
}

/**
 * After `continue_session` + DB resync, use the original durable message id to
 * tell whether Pending resume can retry the turn. Content is not identity.
 */
export function shouldResubmitOriginalUser(
	syncedMessages: ContinueMessage[],
	originalMessageId?: string,
): boolean {
	// Optimistic ids are local to the previous UI send and cannot prove durable
	// acceptance. Resubmit when there is no durable id or the exact id vanished.
	if (!originalMessageId?.startsWith('msg-')) return true;
	return !syncedMessages.some(
		(message) => message.role === 'user' && message.id === originalMessageId,
	);
}
