import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/svelte';
import CountChip from './CountChip.svelte';

describe('CountChip', () => {
	it('floors finite numeric counts for display', () => {
		const { getByText } = render(CountChip, { count: 3.8 });

		expect(getByText('共 3 项')).toBeTruthy();
	});

	it('clamps negative and non-finite numeric counts to zero', () => {
		const { container, rerender } = render(CountChip, { count: -2 });
		expect(container.textContent).toContain('共 0 项');

		rerender({ count: Number.NaN });
		expect(container.textContent).toContain('共 0 项');

		rerender({ count: Number.POSITIVE_INFINITY });
		expect(container.textContent).toContain('共 0 项');
	});
});
