type DraftListener = (text: string) => void;

const listeners = new Set<DraftListener>();

/**
 * Deliver user-approved MCP text into the active chat composer without
 * persisting it or submitting it. The composer remains the editing authority.
 */
export function subscribeMcpDraft(listener: DraftListener): () => void {
	listeners.add(listener);
	return () => listeners.delete(listener);
}

export function offerMcpDraft(text: string): boolean {
	if (text.length === 0 || listeners.size === 0) return false;
	for (const listener of listeners) listener(text);
	return true;
}
