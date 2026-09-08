import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import LongTermFacts from './views/LongTermFacts.svelte';

describe('LongTermFacts', () => {
	it('opens a fact card in a detail dialog', async () => {
		const onDeleteFact = vi.fn();
		const props: any = {
			facts: [
				{
					id: 'fact-1',
					subject: 'user',
					predicate: 'theme',
					object: 'dark',
					source: 'user',
					tags: ['preference'],
					confidence: 0.9,
					mention_count: 2,
					created_at: '2026-09-01T10:00:00Z',
				},
			],
			factsLoaded: true,
			newFact: { predicate: '', object: '', tags: '' },
			onDeleteFact,
		};
		render(LongTermFacts, props);

		expect(screen.queryByRole('heading', { name: 'theme' })).toBeNull();
		await fireEvent.click(screen.getByRole('button', { name: '查看theme详情' }));
		expect(screen.getByRole('dialog')).toBeTruthy();
		expect(screen.getByRole('heading', { name: 'theme' })).toBeTruthy();
		expect(screen.getAllByText('dark').length).toBeGreaterThan(0);
		await fireEvent.click(screen.getByRole('button', { name: '删除这条记忆' }));
		expect(onDeleteFact).toHaveBeenCalledWith('fact-1');
	});
});
