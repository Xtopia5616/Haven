import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/svelte';
import Icon from './Icon.svelte';
import { ICONS } from './icons.ts';

describe('Icon', () => {
	it('renders shared paths with one predictable size and viewBox', () => {
		const { container } = render(Icon, { name: 'copy', size: 16 } as any);
		const svg = container.querySelector('svg.icon');

		expect(svg?.getAttribute('width')).toBe('16');
		expect(svg?.getAttribute('height')).toBe('16');
		expect(svg?.getAttribute('viewBox')).toBe('0 0 24 24');
		expect(svg?.querySelector('rect')).toBeTruthy();
	});

	it('falls back safely for unknown names', () => {
		const { container } = render(Icon, { name: 'not-a-real-icon' } as any);
		const svg = container.querySelector('svg.icon');

		expect(svg?.querySelector('circle')).toBeTruthy();
		expect(svg?.getAttribute('aria-hidden')).toBe('true');
	});

	it('keeps every registered icon non-empty', () => {
		for (const definition of Object.values(ICONS)) {
			expect(definition.body.trim().length).toBeGreaterThan(0);
		}
	});
});
