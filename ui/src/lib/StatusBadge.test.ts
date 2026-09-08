import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import StatusBadge from './StatusBadge.svelte';

describe('StatusBadge', () => {
	it('renders a semantic tone without duplicating badge geometry', () => {
		render(StatusBadge, { label: '运行中', tone: 'success' } as any);

		const badge = screen.getByText('运行中');
		expect(badge.classList.contains('status-badge')).toBe(true);
		expect(badge.getAttribute('data-tone')).toBe('success');
	});
});
