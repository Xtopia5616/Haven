import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
vi.mock('./tauri.ts', () => ({
	invoke: vi.fn().mockResolvedValue(undefined),
}));

import { invoke } from './tauri.ts';
import { notificationStore } from './notificationStore.ts';
import { reportError } from './errorHandling.ts';

describe('errorHandling', () => {
	beforeEach(() => {
		notificationStore.set([]);
		vi.mocked(invoke).mockClear();
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
		expect(invoke).toHaveBeenCalledTimes(1);
		expect(invoke).toHaveBeenCalledWith('log_frontend_error', {
			message: '保存设置失败: backend failed',
		});
		spy.mockRestore();
	});

	it('can add a toast without duplicating a lower-level log', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
		const error = new Error('already logged');

		reportError(error, { context: 'invoke', message: '加载失败', log: false });

		expect(get(notificationStore)[0].msg).toBe('加载失败: already logged');
		expect(spy).not.toHaveBeenCalled();
		expect(invoke).toHaveBeenCalledWith('log_frontend_error', {
			message: '加载失败: already logged',
		});
		spy.mockRestore();
	});

	it('can keep technical details in logs and show a generic toast', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

		reportError(new Error('backend detail'), {
			context: 'global',
			message: 'Unhandled UI error',
			includeDetail: false,
			notificationMessage: '应用发生未处理错误，请重试',
		});

		expect(get(notificationStore)[0].msg).toBe('应用发生未处理错误，请重试');
		expect(spy.mock.calls[0]).toContain('backend detail');
		expect(invoke).toHaveBeenCalledWith('log_frontend_error', {
			message: '应用发生未处理错误，请重试',
		});
		spy.mockRestore();
	});
});
