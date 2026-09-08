import { describe, expect, it } from 'vitest';
import { distanceFromChatBottom, isChatNearBottom } from './chatScroll';

function viewport(overrides: Partial<HTMLElement> = {}) {
	return {
		scrollHeight: 1200,
		scrollTop: 800,
		clientHeight: 360,
		...overrides,
	};
}

describe('chat scroll position helpers', () => {
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
});
