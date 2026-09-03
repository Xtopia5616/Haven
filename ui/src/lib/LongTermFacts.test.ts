import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import LongTermFacts from './views/LongTermFacts.svelte';

describe('LongTermFacts', () => {
	it('keeps the selected fact visible in a detail panel', async () => {
		const onDeleteFact = vi.fn();
		const props: any = {
			facts: [
				{ id: 'fact-1', subject: 'user', predicate: 'theme', object: 'dark', source: 'user', tags: ['preference'] },
			],
			factsLoaded: true,
			newFact: { predicate: '', object: '', tags: '' },
			onDeleteFact,
		};
		render(LongTermFacts, props);

		expect(screen.getByRole('heading', { name: 'theme' })).toBeTruthy();
		expect(screen.getAllByText('dark').length).toBeGreaterThan(0);
		await fireEvent.click(screen.getByRole('button', { name: '删除这条事实' }));
		expect(onDeleteFact).toHaveBeenCalledWith('fact-1');
	});
});
