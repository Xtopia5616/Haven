import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn().mockResolvedValue(undefined),
}));

import { invoke } from './tauri.ts';
import { notificationStore, addNotification } from './notificationStore.ts';

describe('addNotification', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		notificationStore.set([]);
		vi.mocked(invoke).mockClear();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('adds a notification with msg and type', () => {
		addNotification('hello', 'info');
		const items = get(notificationStore);
		expect(items).toHaveLength(1);
		expect(items[0].msg).toBe('hello');
		expect(items[0].type).toBe('info');
		expect(typeof items[0].id).toBe('string');
	});

	it('deduplicates identical msg+type', () => {
		addNotification('same', 'warning');
		addNotification('same', 'warning');
		addNotification('same', 'info');
		expect(get(notificationStore)).toHaveLength(2);
	});

	it('auto-removes after the duration', () => {
		addNotification('temp', 'info', 1000);
		expect(get(notificationStore)).toHaveLength(1);
		vi.advanceTimersByTime(999);
		expect(get(notificationStore)).toHaveLength(1);
		vi.advanceTimersByTime(2);
		expect(get(notificationStore)).toHaveLength(0);
	});

	it('removes only its own notification', () => {
		addNotification('a', 'info', 1000);
		addNotification('b', 'info', 5000);
		vi.advanceTimersByTime(1001);
		const items = get(notificationStore);
		expect(items).toHaveLength(1);
		expect(items[0].msg).toBe('b');
	});

	it('keeps error toasts presentational', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
		addNotification('boom', 'error');
		addNotification('ok', 'info');
		addNotification('oops', 'warning');
		expect(spy).not.toHaveBeenCalled();
		expect(invoke).not.toHaveBeenCalled();
		spy.mockRestore();
	});

	it('does not log or mirror informational notifications', () => {
		addNotification('正在加载…', 'info');

		expect(invoke).not.toHaveBeenCalled();
	});
});
