import { render } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import CountChip from './CountChip.svelte';

describe('CountChip', () => {
	it('renders a reusable total count label', () => {
		render(CountChip, { count: 3, label: '条历史' });

		expect(document.querySelector('.count-chip')?.textContent).toBe('共 3 条历史');
	});

	it('normalizes invalid and fractional counts', () => {
		render(CountChip, { count: -2.8, label: '项' });
		expect(document.querySelector('.count-chip')?.textContent).toBe('共 0 项');
	});
});
