import { describe, expect, it } from 'vitest';
import {
	chatBottomOverlayClearance,
	distanceFromChatBottom,
	isChatNearBottom,
	shouldFollowChatScroll,
} from './chatScroll';

function viewport(overrides: Partial<HTMLElement> = {}) {
	return {
		scrollHeight: 1200,
		scrollTop: 800,
		clientHeight: 360,
		...overrides,
	};
}

describe('chat scroll position helpers', () => {
	it('reserves the real space above a bottom overlay', () => {
		expect(chatBottomOverlayClearance(900, 580)).toBe(320);
		expect(chatBottomOverlayClearance(900, 940)).toBe(0);
	});

	it('keeps the distance from the bottom non-negative', () => {
		expect(distanceFromChatBottom(viewport({ scrollTop: 900 }))).toBe(0);
	});

	it('considers only the last 32 pixels near enough to follow', () => {
		expect(isChatNearBottom(viewport({ scrollTop: 808 }))).toBe(true);
		expect(isChatNearBottom(viewport({ scrollTop: 807 }))).toBe(false);
	});

	it('supports a caller-specific threshold', () => {
		expect(isChatNearBottom(viewport({ scrollTop: 790 }), 60)).toBe(true);
	});

	it('keeps the follow state stable in the near-bottom gap', () => {
		expect(shouldFollowChatScroll(viewport({ scrollTop: 810 }), false)).toBe(false);
		expect(shouldFollowChatScroll(viewport({ scrollTop: 810 }), true)).toBe(true);
		expect(shouldFollowChatScroll(viewport({ scrollTop: 838 }), false)).toBe(true);
	});
});
