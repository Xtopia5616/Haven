import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
vi.mock('./tauri.ts', () => ({
	invoke: vi.fn().mockResolvedValue(undefined),
}));

import { invoke } from './tauri.ts';
import { notificationStore } from './notificationStore.ts';
import { logError, reportError } from './errorHandling.ts';

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
		expect(spy.mock.calls[0][0]).toContain('[ERROR][SettingsView] operation failed');
		expect(invoke).toHaveBeenCalledTimes(1);
		expect(invoke).toHaveBeenCalledWith('log_frontend_error', {
			failureKind: 'operation_failure',
		});
		spy.mockRestore();
	});

	it('can add a toast without duplicating a lower-level log', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
		const error = new Error('already logged');

		reportError(error, { context: 'invoke', message: '加载失败', log: false });

		expect(get(notificationStore)[0].msg).toBe('加载失败: already logged');
		expect(spy).not.toHaveBeenCalled();
		expect(invoke).not.toHaveBeenCalled();
		spy.mockRestore();
	});

	it('does not submit a second backend log for an error already logged at invoke', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
		const error = Object.freeze(new Error('already logged by invoke'));
		logError('invoke', error);

		const message = reportError(error, {
			context: 'externalRef',
			message: '打开失败',
		});

		expect(message).toBe('打开失败: already logged by invoke');
		expect(get(notificationStore)[0].msg).toBe('打开失败: already logged by invoke');
		expect(spy).toHaveBeenCalledTimes(1);
		expect(invoke).not.toHaveBeenCalled();
		spy.mockRestore();
	});

	it('keeps technical details out of logs while showing a generic toast', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

		reportError(new Error('backend detail'), {
			context: 'global',
			message: 'Unhandled UI error',
			includeDetail: false,
			notificationMessage: '应用发生未处理错误，请重试',
			failureKind: 'unhandled_ui_error',
		});

		expect(get(notificationStore)[0].msg).toBe('应用发生未处理错误，请重试');
		expect(spy.mock.calls[0]).not.toContain('backend detail');
		expect(invoke).toHaveBeenCalledWith('log_frontend_error', {
			failureKind: 'unhandled_ui_error',
		});
		spy.mockRestore();
	});
});
