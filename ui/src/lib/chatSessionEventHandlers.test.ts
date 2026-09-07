import { describe, expect, it, vi } from 'vitest';
import { createChatSessionEventHandlers } from './chatSessionEventHandlers.ts';

function handlers(options: {
	fresh?: boolean;
	adoptedDraft?: boolean;
	setActiveSessionId?: (sessionId: string) => void;
}) {
	return createChatSessionEventHandlers({
		getActiveSessionId: () => null,
		isFreshSessionIntent: () => options.fresh ?? false,
		adoptDraftMessages: () => options.adoptedDraft ?? false,
		setActiveSessionId: options.setActiveSessionId ?? vi.fn(),
		getSessionErrorId: () => null,
		clearSessionError: vi.fn(),
		showSessionError: vi.fn(),
		clearAskAwaiting: vi.fn(),
		evictTerminalSessionMemory: vi.fn(),
		clearStepBlockIds: vi.fn(),
		updateSessionTitle: vi.fn(),
		loadSessions: vi.fn(),
	});
}

describe('chat session lifecycle handlers', () => {
	it('selects a fresh session when it adopts the pending draft', () => {
		const setActiveSessionId = vi.fn();
		const eventHandlers = handlers({
			fresh: true,
			adoptedDraft: true,
			setActiveSessionId,
		});

		eventHandlers['session:created']({
			payload: { sessionId: 'ses-fast', status: 'pending', title: null },
		} as never);

		expect(setActiveSessionId).toHaveBeenCalledWith('ses-fast');
	});

	it('does not select an unrelated session while a fresh draft is pending', () => {
		const setActiveSessionId = vi.fn();
		const eventHandlers = handlers({ fresh: true, adoptedDraft: false, setActiveSessionId });

		eventHandlers['session:created']({
			payload: { sessionId: 'ses-background', status: 'pending', title: null },
		} as never);

		expect(setActiveSessionId).not.toHaveBeenCalled();
	});
});
