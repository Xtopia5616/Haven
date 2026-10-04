import { describe, expect, it, vi } from 'vitest';
import { writable } from 'svelte/store';
import { syncStore } from './syncStore.ts';

describe('syncStore', () => {
	it('stops projecting values after the subscription is disposed', () => {
		const store = writable('idle');
		const project = vi.fn();
		const unsubscribe = syncStore(store, project);

		expect(project).toHaveBeenLastCalledWith('idle');
		store.set('generating');
		expect(project).toHaveBeenLastCalledWith('generating');

		unsubscribe();
		store.set('idle');
		expect(project).toHaveBeenCalledTimes(2);
	});
});
