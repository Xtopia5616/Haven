/** Distance in pixels at which the conversation is considered to be at its end. */
export const CHAT_SCROLL_BOTTOM_THRESHOLD = 32;
/** Smaller settled range used to hide the jump button without flickering. */
export const CHAT_SCROLL_SETTLED_THRESHOLD = 4;

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

/**
 * Return the viewport distance occupied by a bottom overlay.
 *
 * Measuring the two edges instead of deriving the value from the overlay's
 * height keeps the scroll clearance correct when the overlay is positioned by
 * CSS (including its bottom offset and any future transforms).
 */
export function chatBottomOverlayClearance(pageBottom: number, overlayTop: number) {
	return Math.max(0, pageBottom - overlayTop);
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

/**
 * Keep the follow state stable in the small gap between the visible bottom
 * threshold and the exact settled position.
 */
export function shouldFollowChatScroll(
	element: {
		scrollHeight: number;
		scrollTop: number;
		clientHeight: number;
	},
	currentlyFollowing: boolean,
) {
	if (isChatNearBottom(element, CHAT_SCROLL_SETTLED_THRESHOLD)) return true;
	if (!isChatNearBottom(element)) return false;
	return currentlyFollowing;
}
