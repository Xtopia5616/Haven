import '@testing-library/svelte/vitest';

// jsdom does not implement the Web Animations API (`element.animate`), which
// Svelte's `in:`/`out:` transitions rely on. Provide a minimal polyfill that
// drives `onfinish` on a timer so components with transitions can mount and
// complete their transitions in tests.
if (
	typeof Element !== 'undefined' &&
	typeof (Element.prototype as { animate?: unknown }).animate !== 'function'
) {
	Object.defineProperty(Element.prototype, 'animate', {
		configurable: true,
		value: function animate(
			_keyframes: Keyframe[] | PropertyIndexedKeyframes,
			options: { duration?: number; delay?: number } = {},
		) {
			const anim = {
				onfinish: null as (() => void) | null,
				playState: 'running',
				effect: null as unknown,
				cancel() {
					this.playState = 'idle';
					this.effect = null;
				},
			};
			const { duration = 0, delay = 0 } = options;
			setTimeout(() => {
				if (anim.playState === 'idle') return;
				anim.playState = 'finished';
				anim.onfinish?.();
			}, duration + delay);
			return anim;
		},
	});
}
