import {
	CHAT_SCROLL_SETTLED_THRESHOLD,
	chatBottomOverlayClearance,
	isChatNearBottom,
	shouldFollowChatScroll,
} from './chatScroll.ts';

export interface ChatViewControllerDependencies {
	getMessagesElement: () => HTMLElement | null | undefined;
	getAutoFollow: () => boolean;
	setAutoFollow: (follow: boolean) => void;
	isDisposed: () => boolean;
	requestFrame?: (callback: FrameRequestCallback) => number;
	setTimer?: typeof setTimeout;
	clearTimer?: typeof clearTimeout;
	createResizeObserver?: (
		callback: ResizeObserverCallback,
	) => Pick<ResizeObserver, 'observe' | 'disconnect'> | null;
}

/** Own DOM scroll, auto-follow and bottom-overlay layout effects for chat. */
export function createChatViewController(dependencies: ChatViewControllerDependencies) {
	let scrollRafPending = false;
	let jumpingToBottom = false;
	let jumpBottomTimer: ReturnType<typeof setTimeout> | null = null;
	let disposed = false;

	const requestFrame =
		dependencies.requestFrame ?? ((callback: FrameRequestCallback) => requestAnimationFrame(callback));
	const setTimer = dependencies.setTimer ?? setTimeout;
	const clearTimer = dependencies.clearTimer ?? clearTimeout;
	const createResizeObserver =
		dependencies.createResizeObserver ??
		((callback: ResizeObserverCallback) =>
			typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(callback));
	const hasResizeObserver =
		dependencies.createResizeObserver !== undefined || typeof ResizeObserver !== 'undefined';

	function isDead() {
		return disposed || dependencies.isDisposed();
	}

	function scrollToBottom() {
		const messages = dependencies.getMessagesElement();
		if (!messages || isDead() || scrollRafPending) return;
		scrollRafPending = true;
		requestFrame(() => {
			scrollRafPending = false;
			// Respect a user scrolling up between scheduling and this frame.
			const currentMessages = dependencies.getMessagesElement();
			if (isDead() || !currentMessages || !dependencies.getAutoFollow()) return;
			currentMessages.scrollTop = currentMessages.scrollHeight;
		});
	}

	/** Render skipped bubbles for two frames to get their real cold-open heights. */
	function scrollToBottomAfterOpen() {
		const messages = dependencies.getMessagesElement();
		if (isDead() || !messages) return;
		const list = messages.querySelector('.message-list');
		if (!list) return;
		const bubbles = Array.from(list.querySelectorAll('.bubble')) as HTMLElement[];
		bubbles.forEach((bubble) => bubble.style.setProperty('content-visibility', 'visible'));
		messages.scrollTop = messages.scrollHeight;
		let frames = 2;
		const finish = () => {
			frames -= 1;
			if (frames > 0) {
				requestFrame(finish);
				return;
			}
			const currentMessages = dependencies.getMessagesElement();
			if (isDead() || !currentMessages) return;
			if (dependencies.getAutoFollow()) currentMessages.scrollTop = currentMessages.scrollHeight;
			bubbles.forEach((bubble) => bubble.style.removeProperty('content-visibility'));
		};
		requestFrame(finish);
	}

	function stopJumpToBottom() {
		jumpingToBottom = false;
		if (jumpBottomTimer) {
			clearTimer(jumpBottomTimer);
			jumpBottomTimer = null;
		}
	}

	function onScroll() {
		const messages = dependencies.getMessagesElement();
		if (!messages) return;
		const settledAtBottom = isChatNearBottom(messages, CHAT_SCROLL_SETTLED_THRESHOLD);
		// Keep the jump affordance hidden while a smooth scroll is settling.
		if (jumpingToBottom) {
			if (settledAtBottom) stopJumpToBottom();
			return;
		}
		dependencies.setAutoFollow(shouldFollowChatScroll(messages, dependencies.getAutoFollow()));
	}

	function cancelJumpToBottom() {
		if (!jumpingToBottom) return;
		stopJumpToBottom();
		const messages = dependencies.getMessagesElement();
		if (messages) dependencies.setAutoFollow(isChatNearBottom(messages));
	}

	function jumpToBottom() {
		const messages = dependencies.getMessagesElement();
		if (!messages) return;
		stopJumpToBottom();
		dependencies.setAutoFollow(true);
		jumpingToBottom = true;
		messages.scrollTo({ top: messages.scrollHeight, behavior: 'smooth' });
		// WebViews normally emit the final scroll event; the timeout releases the
		// guard if the target is already at the end or events get coalesced.
		jumpBottomTimer = setTimer(() => {
			stopJumpToBottom();
			const currentMessages = dependencies.getMessagesElement();
			if (currentMessages) dependencies.setAutoFollow(isChatNearBottom(currentMessages));
		}, 700);
	}

	function observeComposerClearance(page: HTMLElement | null, browser: boolean) {
		if (!browser || !page || !hasResizeObserver) return;
		const composer = page.querySelector('.input-area') as HTMLElement | null;
		if (!composer) return;
		const update = () => {
			const pageRect = page.getBoundingClientRect();
			const composerRect = composer.getBoundingClientRect();
			const clearance = chatBottomOverlayClearance(pageRect.bottom, composerRect.top);
			page.style.setProperty('--chat-composer-clearance', `${clearance}px`);
			// The clearance padding changes scrollHeight. Preserve follow intent.
			if (dependencies.getAutoFollow()) scrollToBottom();
		};
		const observer = createResizeObserver(update);
		if (!observer) return;
		observer.observe(composer);
		observer.observe(page);
		update();
		return () => {
			observer.disconnect();
			page.style.removeProperty('--chat-composer-clearance');
		};
	}

	function dispose() {
		disposed = true;
		stopJumpToBottom();
	}

	return {
		cancelJumpToBottom,
		dispose,
		jumpToBottom,
		onScroll,
		observeComposerClearance,
		scrollToBottom,
		scrollToBottomAfterOpen,
		setAutoFollow: dependencies.setAutoFollow,
	};
}
