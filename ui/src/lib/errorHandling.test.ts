import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { notificationStore } from './stores.ts';
import { reportError } from './errorHandling.ts';

describe('errorHandling', () => {
	beforeEach(() => {
		notificationStore.set([]);
	});

	it('logs and displays a normalized error through one entry point', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

		const message = reportError(new Error('backend failed'), {
			context: 'SettingsView',
			message: '保存设置失败',
		});

		expect(message).toBe('保存设置失败: backend failed');
		expect(get(notificationStore)).toMatchObject([
			{ msg: '保存设置失败: backend failed', type: 'error' },
		]);
		expect(spy).toHaveBeenCalledTimes(1);
		expect(spy.mock.calls[0][0]).toContain('[ERROR][SettingsView] 保存设置失败 failed');
		spy.mockRestore();
	});

	it('can add a toast without duplicating a lower-level log', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
		const error = new Error('already logged');

		reportError(error, { context: 'invoke', message: '加载失败', log: false });

		expect(get(notificationStore)[0].msg).toBe('加载失败: already logged');
		expect(spy).not.toHaveBeenCalled();
		spy.mockRestore();
	});
});
