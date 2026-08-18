import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/svelte';
import MaterialBadge from './MaterialBadge.svelte';

describe('MaterialBadge', () => {
	it('renders its text with the default variant', () => {
		render(MaterialBadge, { text: 'Label' });
		const badge = document.querySelector('.md-badge');
		expect(badge.textContent).toBe('Label');
		expect(badge.getAttribute('data-variant')).toBe('default');
	});

	it('applies a custom variant', () => {
		render(MaterialBadge, { text: 'Done', variant: 'success' });
		const badge = document.querySelector('.md-badge');
		expect(badge.getAttribute('data-variant')).toBe('success');
	});
});
