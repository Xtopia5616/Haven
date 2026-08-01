import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/svelte';
import StatusDot from './StatusDot.svelte';

describe('StatusDot', () => {
	const dot = () => /** @type {HTMLElement} */ (document.querySelector('.status-dot'));

	it('defaults to the success color and no animation', () => {
		render(StatusDot, {});
		expect(dot()).toBeTruthy();
		expect(dot().style.getPropertyValue('--dot-color')).toBe('var(--md-sys-color-success)');
		expect(dot().classList.contains('animate')).toBe(false);
	});

	it('applies the requested color', () => {
		render(StatusDot, { color: 'warning' });
		expect(dot().style.getPropertyValue('--dot-color')).toBe('var(--md-sys-color-warning)');
	});

	it('adds the animate class when requested', () => {
		render(StatusDot, { animate: true });
		expect(dot().classList.contains('animate')).toBe(true);
	});
});
