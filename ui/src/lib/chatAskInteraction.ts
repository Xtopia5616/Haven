import { get } from 'svelte/store';
import { sessionMessagesStore, updateSessionMessages } from './stores.ts';

interface AskMessage {
	id: string;
	type?: string;
	content?: string;
	awaiting?: boolean;
	resolved?: { answer?: string; ignored?: boolean } | null;
}

interface AskInteractionContext {
	getActiveSessionId: () => string | null;
	setAutoFollow: () => void;
	setSelectionsReady: (ready: boolean) => void;
	submitMessage: (text: string, images?: unknown, files?: unknown) => void;
}

interface ResolveAskOptions {
	deferSubmit?: boolean;
}

/**
 * Own the chat-side ask batching state. The route supplies the active-session
 * and submission callbacks, while this controller keeps option selections,
 * resolved ids and duplicate-submit protection together.
 */
export function createAskInteractionController({
	getActiveSessionId,
	setAutoFollow,
	setSelectionsReady,
	submitMessage,
}: AskInteractionContext) {
	const askSelections = new Map<string, Map<string, string[]>>();
	const resolvedAskIds = new Map<string, Set<string>>();

	const messagesFor = (sessionId: string): AskMessage[] =>
		(get(sessionMessagesStore)[sessionId] || []) as AskMessage[];

	function computeAskSelectionsReady() {
		const sessionId = getActiveSessionId();
		if (!sessionId) return false;
		const awaiting = messagesFor(sessionId).filter(
			(message) => message.type === 'ask' && message.awaiting,
		);
		if (awaiting.length === 0) return false;
		const byMessage = askSelections.get(sessionId);
		if (!byMessage) return false;
		return awaiting.every((message) => (byMessage.get(message.id) || []).length > 0);
	}

	function refreshSelectionsReady() {
		setSelectionsReady(computeAskSelectionsReady());
	}

	function clearAskSelections(sessionId: string) {
		askSelections.delete(sessionId);
		refreshSelectionsReady();
	}

	function clearAskAwaiting(sessionId: string) {
		updateSessionMessages(sessionId, (messages) =>
			messages.map((message) =>
				message.type === 'ask'
					? { ...message, awaiting: false, resolved: null }
					: message,
			),
		);
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

	function submitActionAnswers(
		sessionId: string,
		resolvedIds: Set<string> | undefined,
		extraText = '',
		images: unknown = [],
		files: unknown = [],
	) {
		if (!resolvedIds || resolvedIds.size === 0) return;
		const asks = messagesFor(sessionId).filter(
			(message) => message.type === 'ask' && message.resolved && resolvedIds.has(message.id),
		);
		if (asks.length === 0) return;
		const single = asks.length === 1;
		let text = asks
			.map((message, index) => {
				const answer = message.resolved?.ignored ? '忽略' : message.resolved?.answer || '';
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
		resolved: { answer?: string; ignored?: boolean },
		opts: ResolveAskOptions = {},
	) {
		const sessionId = getActiveSessionId();
		if (!sessionId || !msgId) return;
		const ids = resolvedAskIds.get(sessionId) || new Set<string>();
		// A double-click must not compose and submit the same answer twice.
		if (ids.has(msgId)) return;
		updateSessionMessages(sessionId, (messages) =>
			messages.map((message) =>
				message.id === msgId && message.type === 'ask' && !message.resolved
					? { ...message, awaiting: false, resolved }
					: message,
			),
		);
		ids.add(msgId);
		resolvedAskIds.set(sessionId, ids);
		const byMessage = askSelections.get(sessionId);
		if (byMessage) {
			byMessage.delete(msgId);
			if (byMessage.size === 0) askSelections.delete(sessionId);
		}
		refreshSelectionsReady();
		if (opts.deferSubmit) return;
		const remaining = messagesFor(sessionId).filter(
			(message) => message.type === 'ask' && message.awaiting,
		);
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
		const awaiting = messagesFor(sessionId).filter(
			(message) => message.type === 'ask' && message.awaiting,
		);
		if (awaiting.length === 0) return false;
		const byMessage = askSelections.get(sessionId);
		if (!byMessage) return false;
		if (!awaiting.every((message) => (byMessage.get(message.id) || []).length > 0)) return false;
		for (const ask of awaiting) {
			const selected = byMessage.get(ask.id) || [];
			resolveAsk(ask.id, { answer: selected.join(' ') }, { deferSubmit: true });
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

	function handleIgnoreAsk(msgId: string) {
		if (!getActiveSessionId()) return;
		resolveAsk(msgId, { ignored: true });
	}

	return {
		clearAskAwaiting,
		computeAskSelectionsReady,
		handleAskSelectionChange,
		handleAskSubmit,
		handleIgnoreAsk,
		trySubmitAskSelections,
	};
}
