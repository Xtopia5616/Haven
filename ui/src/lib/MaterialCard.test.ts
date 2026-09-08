import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import MaterialCard from './MaterialCard.svelte';

	describe('MaterialCard', () => {
	it('renders the shared surface variant and content', () => {
		render(MaterialCard, { variant: 'outlined', className: 'settings-card' } as any);

		const card = document.querySelector('.md-card');
		expect(card?.classList.contains('md-card')).toBe(true);
		expect(card?.classList.contains('md-card--outlined')).toBe(true);
		expect(card?.classList.contains('settings-card')).toBe(true);
	});
});
