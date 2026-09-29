import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SettingsView from './SettingsView.svelte';

const { invoke, listen } = vi.hoisted(() => ({
	invoke: vi.fn(),
	listen: vi.fn(),
}));

import {
	DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS,
	setActionCompletionNotificationChannels,
	shouldShowActionCompletionInApp,
} from '$lib/actionCompletionNotificationSettings.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke, listen }));

describe('SettingsView diagnostics export', () => {
	beforeEach(() => {
		setActionCompletionNotificationChannels(DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS);
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					return null;
				case 'get_api_key_status':
					return { models: {}, providers: {}, stt: false, ocr: false, ocr_secret: false };
				case 'is_autostart_enabled':
					return false;
				case 'get_performance_metrics':
					return { backend: { requests: 1 }, renderer: { frames: 2 } };
				default:
					return [];
			}
		});
		listen.mockResolvedValue(() => {});
	});

	it('creates and clicks a JSON download when the export button is pressed', async () => {
		const createObjectURL = vi.fn<(blob: Blob) => string>(() => 'blob:performance-metrics');
		const revokeObjectURL = vi.fn();
		Object.defineProperty(URL, 'createObjectURL', {
			configurable: true,
			value: createObjectURL,
		});
		Object.defineProperty(URL, 'revokeObjectURL', {
			configurable: true,
			value: revokeObjectURL,
		});
		const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});

		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await fireEvent.click(screen.getByRole('button', { name: '导出性能指标' }));

		expect(invoke).toHaveBeenCalledWith('get_performance_metrics', undefined);
		expect(createObjectURL).toHaveBeenCalledOnce();
		const blob = createObjectURL.mock.calls[0][0] as Blob;
		expect(await blob.text()).toContain('"requests": 1');
		expect(click).toHaveBeenCalledOnce();
		const link = click.mock.instances[0] as HTMLAnchorElement;
		expect(link.download).toMatch(/^haven-performance-metrics-.*\.json$/);
		expect(revokeObjectURL).toHaveBeenCalledWith('blob:performance-metrics');

		click.mockRestore();
	});

	it('keeps permission management on its own settings tab', async () => {
		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));

		await fireEvent.click(screen.getByRole('tab', { name: /权限/ }));
		expect(screen.getByRole('heading', { name: '权限中心' })).toBeTruthy();
	});

	it('persists action completion channels independently and applies the toast switch', async () => {
		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('is_autostart_enabled'));
		await new Promise((resolve) => setTimeout(resolve, 0));

		const inApp = await screen.findByRole('switch', { name: '任务完成应用内提示' });
		const windows = screen.getByRole('switch', { name: '任务完成 Windows 通知' });
		expect((inApp as HTMLInputElement).checked).toBe(true);
		expect((windows as HTMLInputElement).checked).toBe(true);

		await fireEvent.click(inApp);
		await waitFor(() => expect(screen.getByRole('button', { name: '保存设置' })).toBeTruthy());
		expect((windows as HTMLInputElement).checked).toBe(true);
		await fireEvent.click(windows);
		expect((windows as HTMLInputElement).checked).toBe(false);
		await fireEvent.click(screen.getByRole('button', { name: '保存设置' }));

		await waitFor(() => {
			expect(invoke).toHaveBeenCalledWith(
				'update_settings',
				expect.objectContaining({
					settings: expect.objectContaining({
						notification: expect.objectContaining({
							action_completed: { in_app: false, windows: false },
						}),
					}),
				}),
			);
		});
		expect(shouldShowActionCompletionInApp()).toBe(false);
	});

	it('stages provider keys separately and omits values from Settings updates', async () => {
		invoke.mockClear();
		listen.mockClear();
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					return { llm: { providers: [], models: [], request_policies: [] } };
				case 'stage_provider_credential':
					return 'cred-0123456789abcdef0123456789abcdef';
				case 'get_api_key_status':
					return { models: {}, providers: {}, stt: false, ocr: false, ocr_secret: false };
				case 'is_autostart_enabled':
					return false;
				case 'check_shell_available':
					return { available: true };
				default:
					return [];
			}
		});

		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await fireEvent.click(screen.getByRole('tab', { name: /模型/ }));
		await fireEvent.click(await screen.findByRole('button', { name: '添加 Provider' }));
		await fireEvent.input(screen.getByPlaceholderText('唯一名称，角色据此选择'), {
			target: { value: 'primary' },
		});
		await fireEvent.input(screen.getByPlaceholderText('sk-...'), {
			target: { value: 'provider-secret-marker' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));
		await fireEvent.click(await screen.findByRole('button', { name: '保存设置' }));

		await waitFor(() => expect(invoke).toHaveBeenCalledWith('update_settings', expect.anything()));
		expect(invoke).toHaveBeenCalledWith('stage_provider_credential', {
			providerName: 'primary',
			apiKey: 'provider-secret-marker',
		});
		const update = invoke.mock.calls.find(([command]) => command === 'update_settings');
		const settings = update?.[1]?.settings;
		expect(settings.llm.providers[0].api_key).toBeUndefined();
		expect(settings.llm.providers[0].api_key_ref).toBe('cred-0123456789abcdef0123456789abcdef');
		expect(JSON.stringify(settings)).not.toContain('provider-secret-marker');
	});
});
