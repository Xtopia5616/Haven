import {
	DRAFT_SESSION_ID,
	type SessionMessage,
	type SessionReducerState,
} from './sessionReducer.ts';

/** Select the active conversation's messages with ask interaction state projected by id. */
export function selectChatVisibleMessages(
	state: SessionReducerState,
	activeSessionId: string | null,
): SessionMessage[] {
	const sessionId = activeSessionId || DRAFT_SESSION_ID;
	const messages = state.messages[sessionId] || [];
	const interactions = state.interactions || {};
	return projectChatVisibleMessages(messages, interactions);
}

/** Project a selected transcript slice with its current ask-interaction state. */
export function projectChatVisibleMessages(
	messages: SessionMessage[],
	interactions: SessionReducerState['interactions'],
): SessionMessage[] {
	return messages.map((message) => {
		if (message.type !== 'ask') return message;

		const request = interactions[message.id];
		if (!request || request.kind !== 'ask') return message;

		const response = request.response as { answer?: string; ignored?: boolean } | undefined;
		return {
			...message,
			options: request.options,
			awaiting: request.status === 'pending',
			resolved:
				request.status === 'resolved'
					? response?.ignored
						? { ignored: true }
						: { answer: response?.answer || '' }
					: null,
		};
	});
}
