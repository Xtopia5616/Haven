import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import AppShell from './AppShell.svelte';

describe('AppShell', () => {
	it('returns to the chat workspace when the Haven logo is clicked', async () => {
		const onNavigate = vi.fn();
		render(AppShell, { activeTab: 'memory', tabs: [], onNavigate } as any);

		await fireEvent.click(screen.getByRole('button', { name: '回到对话' }));

		expect(onNavigate).toHaveBeenCalledWith('chat');
	});
});
