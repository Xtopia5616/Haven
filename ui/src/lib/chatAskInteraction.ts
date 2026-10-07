import type { SessionMessage, SessionReducer } from './sessionReducer.ts';
import type { InteractionKind } from './contracts/generatedCommands.ts';
import type { AskResponseView } from './contracts/app.ts';

interface AskInteractionContext {
	getActiveSessionId: () => string | null;
	setAutoFollow: () => void;
	setSelectionsReady: (ready: boolean) => void;
	submitMessage: (text: string, images?: unknown, files?: unknown) => void;
	reducer: SessionReducer;
}

interface ResolveAskOptions {
	deferSubmit?: boolean;
}

interface InputSubmitPayload {
	text: string;
	images: unknown;
	files: unknown;
}

/**
 * Own the chat-side ask batching state. The route supplies the active-session
 * and submission callbacks, while this controller keeps option selections,
 * current-batch ids and duplicate-submit protection together.
 */
export function createAskInteractionController({
	getActiveSessionId,
	setAutoFollow,
	setSelectionsReady,
	submitMessage,
	reducer,
}: AskInteractionContext) {
	const askSelections = new Map<string, Map<string, string[]>>();
	const resolvedAskIds = new Map<string, Set<string>>();

	const messagesFor = (sessionId: string): SessionMessage[] => reducer.getMessages(sessionId);

	const pendingFor = (sessionId: string, kind: InteractionKind) =>
		Object.values(reducer.snapshot().interactions || {}).filter(
			(request) =>
				request.owner.kind === 'session' &&
				request.owner.sessionId === sessionId &&
				request.sessionId === sessionId &&
				request.kind === kind &&
				request.status === 'pending',
		);

	function computeAskSelectionsReady() {
		const sessionId = getActiveSessionId();
		if (!sessionId) return false;
		const awaiting = pendingFor(sessionId, 'ask');
		if (awaiting.length === 0) return false;
		const byMessage = askSelections.get(sessionId);
		if (!byMessage) return false;
		return awaiting.every((request) => (byMessage.get(request.id) || []).length > 0);
	}

	function refreshSelectionsReady() {
		setSelectionsReady(computeAskSelectionsReady());
	}

	function clearAskSelections(sessionId: string) {
		askSelections.delete(sessionId);
		refreshSelectionsReady();
	}

	function clearAskAwaiting(sessionId: string | null) {
		if (!sessionId) {
			askSelections.clear();
			resolvedAskIds.clear();
			setSelectionsReady(false);
			return;
		}
		const state = reducer.snapshot();
		const asks = messagesFor(sessionId)
			.filter(
				(message) =>
					message.type === 'ask' &&
					(message.awaiting || state.interactions[message.id]?.kind === 'ask'),
			)
			.map((message) => {
				const request = state.interactions[message.id];
				const response =
					request?.owner.kind === 'session' &&
					request.owner.sessionId === sessionId &&
					request.kind === 'ask' &&
					request.status === 'resolved'
						? (request.response as AskResponseView | undefined)
						: undefined;
				return { id: message.id, resolved: response || null };
			});
		reducer.dispatch({ type: 'session/asks-settled', sessionId, asks });
		// A resume/end invalidates quick-reply answers for the pending batch.
		resolvedAskIds.delete(sessionId);
		clearAskSelections(sessionId);
	}

	function handleAskSelectionChange(msgId: string, selected: string[] | null | undefined) {
		const sessionId = getActiveSessionId();
		if (!sessionId || !msgId) return;
		const byMessage = askSelections.get(sessionId) || new Map<string, string[]>();
		if (!selected || selected.length === 0) byMessage.delete(msgId);
		else byMessage.set(msgId, [...selected]);
		if (byMessage.size === 0) askSelections.delete(sessionId);
		else askSelections.set(sessionId, byMessage);
		refreshSelectionsReady();
	}

	function getAskSelection(msgId: string) {
		const sessionId = getActiveSessionId();
		return sessionId ? [...(askSelections.get(sessionId)?.get(msgId) || [])] : [];
	}

	function resolvedResponseFor(sessionId: string, msgId: string) {
		const request = reducer.snapshot().interactions[msgId];
		if (
			request?.owner.kind !== 'session' ||
			request.owner.sessionId !== sessionId ||
			request.sessionId !== sessionId ||
			request.kind !== 'ask' ||
			request.status !== 'resolved'
		)
			return undefined;
		return request.response as AskResponseView | undefined;
	}

	function submitActionAnswers(
		sessionId: string,
		resolvedIds: Set<string> | undefined,
		extraText = '',
		images: unknown = [],
		files: unknown = [],
	) {
		if (!resolvedIds || resolvedIds.size === 0) return;
		const messages = messagesFor(sessionId);
		const asks = messages
			.filter((message) => message.type === 'ask' && resolvedIds.has(message.id))
			.map((message) => ({ message, resolved: resolvedResponseFor(sessionId, message.id) }))
			.filter((entry) => entry.resolved);
		if (asks.length === 0) return;
		const single = asks.length === 1;
		let text = asks
			.map(({ message, resolved }, index) => {
				const answer = resolved?.ignored ? '忽略' : resolved?.answer || '';
				return single
					? answer
					: `关于「${message.content || `问题 ${index + 1}`}」：${answer}`;
			})
			.join('\n');
		const extra = (extraText || '').trim();
		if (extra) text = text ? `${text} ${extra}` : extra;
		setAutoFollow();
		submitMessage(text, images, files);
	}

	function resolveAsk(
		msgId: string,
		resolved: AskResponseView,
		opts: ResolveAskOptions = {},
	) {
		const sessionId = getActiveSessionId();
		if (!sessionId || !msgId) return;
		const ids = resolvedAskIds.get(sessionId) || new Set<string>();
		// A double-click must not compose and submit the same answer twice.
		if (ids.has(msgId)) return;
		const response = resolved.ignored ? { ignored: true } : { answer: resolved.answer || '' };
		reducer.dispatch({ type: 'session/interaction-resolved', id: msgId, response });
		ids.add(msgId);
		resolvedAskIds.set(sessionId, ids);
		const byMessage = askSelections.get(sessionId);
		if (byMessage) {
			byMessage.delete(msgId);
			if (byMessage.size === 0) askSelections.delete(sessionId);
		}
		refreshSelectionsReady();
		if (opts.deferSubmit) return;
		const remaining = pendingFor(sessionId, 'ask');
		if (remaining.length === 0) {
			const submitted = resolvedAskIds.get(sessionId);
			submitActionAnswers(sessionId, submitted);
		}
	}

	function trySubmitAskSelections(
		sessionId: string,
		extraText: string,
		images: unknown,
		files: unknown,
	) {
		const awaiting = pendingFor(sessionId, 'ask');
		if (awaiting.length === 0) return false;
		const byMessage = askSelections.get(sessionId);
		if (!byMessage) return false;
		if (!awaiting.every((message) => (byMessage.get(message.id) || []).length > 0))
			return false;
		for (const request of awaiting) {
			const selected = byMessage.get(request.id) || [];
			resolveAsk(request.id, { answer: selected.join(' ') }, { deferSubmit: true });
		}
		const submitted = resolvedAskIds.get(sessionId);
		clearAskSelections(sessionId);
		submitActionAnswers(sessionId, submitted, extraText, images, files);
		return true;
	}

	function handleAskSubmit() {
		const sessionId = getActiveSessionId();
		if (!sessionId) return;
		setAutoFollow();
		trySubmitAskSelections(sessionId, '', [], []);
	}

	/** Route composer submissions through the pending ask batch when it is ready. */
	function handleInputSubmit({ text, images, files }: InputSubmitPayload) {
		setAutoFollow();
		const sessionId = getActiveSessionId();
		if (sessionId && trySubmitAskSelections(sessionId, text, images, files)) return;
		submitMessage(text, images, files);
	}

	function handleIgnoreAsk(msgId: string) {
		if (!getActiveSessionId()) return;
		resolveAsk(msgId, { ignored: true });
	}

	return {
		clearAskAwaiting,
		computeAskSelectionsReady,
		handleAskSelectionChange,
		getAskSelection,
		handleAskSubmit,
		handleInputSubmit,
		handleIgnoreAsk,
		trySubmitAskSelections,
	};
}
