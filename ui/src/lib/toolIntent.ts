/**
 * Shared user-facing label policy for tool calls and detached actions.
 *
 * Model-written preambles are preferred when they exist. This fallback is
 * deterministic and intentionally generic: the raw command remains available
 * in task details, but never becomes the primary task title by accident.
 */
export const TOOL_INTENT_FALLBACK = '调用工具';

/** Detect a preamble in one live ReAct thought block. */
export function hasToolPreambleInBlock(
	messages: Array<{ id: string; role?: string; type?: string | null; content?: string }>,
	blockId: string | null | undefined,
): boolean {
	if (!blockId) return false;
	const prefix = `${blockId}-`;
	return messages.some(
		(message) =>
			(message.id === blockId || message.id.startsWith(prefix)) &&
			message.role === 'assistant' &&
			message.type !== 'reasoning' &&
			message.type !== 'tool' &&
			message.type !== 'ask' &&
			!!message.content?.trim(),
	);
}

/** Return the first non-empty user-provided action description. */
export function actionIntentLabel(action: { title?: string; body?: string }): string {
	for (const candidate of [action.title, action.body]) {
		if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
	}
	return TOOL_INTENT_FALLBACK;
}

/**
 * Detect a visible assistant preamble immediately before a tool card.
 * Reasoning is deliberately excluded: it is not a user-facing operation
 * summary and must not suppress the deterministic fallback.
 */
export function hasToolPreambleBefore(
	messages: Array<{ role?: string; type?: string | null; content?: string }>,
	toolIndex: number,
): boolean {
	for (let index = toolIndex - 1; index >= 0; index -= 1) {
		const message = messages[index];
		if (message.role === 'user') return false;
		if (message.type === 'tool' || message.type === 'ask') return false;
		if (message.type === 'reasoning') continue;
		if (message.role === 'assistant' && message.content?.trim()) return true;
	}
	return false;
}
