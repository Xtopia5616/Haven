/** Distance in pixels at which the conversation is considered to be at its end. */
export const CHAT_SCROLL_BOTTOM_THRESHOLD = 32;

/**
 * Return the remaining scrollable distance below a conversation viewport.
 * Clamping keeps fractional/overscrolled browser values from producing a
 * negative distance.
 */
export function distanceFromChatBottom(element: {
	scrollHeight: number;
	scrollTop: number;
	clientHeight: number;
}) {
	return Math.max(0, element.scrollHeight - element.scrollTop - element.clientHeight);
}

/** Whether the conversation viewport is close enough to its end to follow it. */
export function isChatNearBottom(
	element: {
		scrollHeight: number;
		scrollTop: number;
		clientHeight: number;
	},
	threshold = CHAT_SCROLL_BOTTOM_THRESHOLD,
) {
	return distanceFromChatBottom(element) <= threshold;
}
