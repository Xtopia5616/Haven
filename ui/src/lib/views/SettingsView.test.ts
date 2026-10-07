import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SettingsView from './SettingsView.svelte';

const { invoke, listen } = vi.hoisted(() => ({
	invoke: vi.fn(),
	listen: vi.fn(),
}));

import {
	DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS,
	setToolRunCompletionNotificationChannels,
	shouldShowToolRunCompletionInApp,
} from '$lib/toolRunCompletionNotificationSettings.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke, listen }));

describe('SettingsView diagnostics export', () => {
	beforeEach(() => {
		setToolRunCompletionNotificationChannels(DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS);
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

	it('keeps the settings page clear when no model provider is configured', async () => {
		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('is_autostart_enabled'));

		expect(screen.queryByText('模型尚未配置')).toBeNull();
	});

	it('syncs the selected chat model into the matching model settings card', async () => {
		const settings = {
			llm: {
				providers: [
					{
						name: 'primary',
						provider: 'openai',
						api_style: 'openai-chat',
						base_url: 'https://example.test/v1',
						api_key_ref: null,
						auth_header_name: 'Authorization',
						auth_header_prefix: 'Bearer',
						proxy_url: null,
						no_proxy: null,
					},
				],
				models: [
					{
						id: 'chat-slot',
						provider_name: 'primary',
						model: 'before-switch',
						capabilities: ['chat'],
					},
				],
				request_policies: [{ request: 'chat', primary: 'chat-slot' }],
				max_concurrent_requests: 2,
			},
		};
		const handlers = new Map<string, (event: unknown) => void>();
		listen.mockImplementation(async (event: string, handler: (event: unknown) => void) => {
			handlers.set(event, handler);
			return () => {};
		});
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					return settings;
				case 'get_api_key_status':
					return { models: {}, providers: {}, stt: false, ocr: false, ocr_secret: false };
				case 'is_autostart_enabled':
					return false;
				default:
					return [];
			}
		});

		const { container } = render(SettingsView);
		await waitFor(() => expect(handlers.has('llm:config_changed')).toBe(true));
		await fireEvent.click(screen.getByRole('tab', { name: /模型与连接/ }));
		await waitFor(() =>
			expect(container.querySelector('.model-provider-name')?.textContent).toContain(
				'before-switch',
			),
		);

		settings.llm.models[0].model = 'after-switch';
		handlers.get('llm:config_changed')?.({
			event: 'llm:config_changed',
			id: 1,
			payload: null,
		});
		await waitFor(() =>
			expect(container.querySelector('.model-provider-name')?.textContent).toContain(
				'after-switch',
			),
		);
	});

	it('shows a chat route created by switching models when no route existed', async () => {
		const settings = {
			llm: {
				providers: [
					{
						name: 'primary',
						provider: 'openai',
						api_style: 'openai-chat',
						base_url: 'https://example.test/v1',
						api_key_ref: null,
						auth_header_name: 'Authorization',
						auth_header_prefix: 'Bearer',
						proxy_url: null,
						no_proxy: null,
					},
				],
				models: [
					{
						id: 'chat-slot',
						provider_name: 'primary',
						model: 'gpt-test',
						capabilities: ['chat'],
					},
				],
				request_policies: [] as Array<{ request: 'chat'; primary: string }>,
				max_concurrent_requests: 2,
			},
		};
		const handlers = new Map<string, (event: unknown) => void>();
		listen.mockImplementation(async (event: string, handler: (event: unknown) => void) => {
			handlers.set(event, handler);
			return () => {};
		});
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					return settings;
				case 'get_api_key_status':
					return { models: {}, providers: {}, stt: false, ocr: false, ocr_secret: false };
				case 'is_autostart_enabled':
					return false;
				default:
					return [];
			}
		});

		const { container } = render(SettingsView);
		await waitFor(() => expect(handlers.has('llm:config_changed')).toBe(true));
		await fireEvent.click(screen.getByRole('tab', { name: /模型与连接/ }));
		expect(container.querySelector('.policy-card')).toBeNull();

		settings.llm.request_policies = [{ request: 'chat', primary: 'chat-slot' }];
		handlers.get('llm:config_changed')?.({
			event: 'llm:config_changed',
			id: 1,
			payload: null,
		});

		await waitFor(() =>
			expect(container.querySelector('.policy-card')?.textContent).toContain('chat'),
		);
		expect(screen.getByRole('button', { name: 'chat 的首选模型' }).textContent).toContain(
			'chat-slot · primary / gpt-test',
		);
	});

	it('suppresses the recording shortcut while capturing a replacement hotkey', async () => {
		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await fireEvent.click(await screen.findByRole('tab', { name: /对话与行为/ }));

		await fireEvent.click(screen.getByRole('button', { name: '快捷键绑定' }));
		expect(invoke).toHaveBeenCalledWith('set_hotkey_capture_active', { active: true });

		await fireEvent.keyDown(window, { key: 'k', code: 'KeyK', ctrlKey: true });
		expect(invoke).toHaveBeenCalledWith('set_hotkey_capture_active', { active: false });
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
		await fireEvent.click(screen.getByRole('tab', { name: /日志与诊断/ }));
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

		await fireEvent.click(screen.getByRole('tab', { name: /安全与权限/ }));
		expect(screen.getByRole('heading', { name: '权限中心' })).toBeTruthy();
	});

	it('persists ToolRun completion channels independently and applies the toast switch', async () => {
		const { container } = render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('is_autostart_enabled'));
		await new Promise((resolve) => setTimeout(resolve, 0));
		await fireEvent.click(screen.getByRole('tab', { name: /界面与通知/ }));

		const inApp = await screen.findByRole('switch', { name: '任务完成应用内提示' });
		const windows = screen.getByRole('switch', { name: '任务完成 Windows 通知' });
		expect((inApp as HTMLInputElement).checked).toBe(true);
		expect((windows as HTMLInputElement).checked).toBe(true);

		await fireEvent.click(inApp);
		await waitFor(() =>
			expect(screen.getByRole('button', { name: '保存' })).toBeTruthy(),
		);
		expect(container.querySelector('.save-bar--bottom-edge')).toBeTruthy();
		expect(container.querySelectorAll('.save-actions .save-action-btn')).toHaveLength(2);
		expect((windows as HTMLInputElement).checked).toBe(true);
		await fireEvent.click(windows);
		expect((windows as HTMLInputElement).checked).toBe(false);
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));

		await waitFor(() => {
			expect(invoke).toHaveBeenCalledWith(
				'update_settings',
				expect.objectContaining({
					settings: expect.objectContaining({
						notification: expect.objectContaining({
							tool_run_completed: { in_app: false, windows: false },
						}),
					}),
				}),
			);
		});
		expect(shouldShowToolRunCompletionInApp()).toBe(false);
	});

	it('preserves backend settings that are outside the visible category during a full save', async () => {
		invoke.mockClear();
		const settings = {
			default_shell: 'powershell',
			llm: { providers: [], models: [], request_policies: [], max_concurrent_requests: 2 },
			hotkey: { mode: 'toggle', key_binding: 'Ctrl+Shift+Space', mute_hotkey: 'Ctrl+M' },
			session: {
				max_concurrent: 3,
				max_steps: 500,
				history_retention_days: 90,
				session_max_steps: 750,
			},
			context_limits: {
				compaction_ratio: 0.65,
				tool_run_result_context_chars: 4321,
				fact_extraction_min_interval_secs: 77,
				turn_deadline_secs: 901,
				max_attachment_images: 4,
				max_attachment_files: 5,
				max_attachment_image_bytes: 10 * 1024 * 1024,
				max_attachment_file_bytes: 20 * 1024 * 1024,
				max_upload_total_bytes: 512 * 1024 * 1024,
				max_attachment_image_dim_px: 1568,
				attachment_image_jpeg_quality: 0.85,
			},
			memory: { session_window_size: 50, fact_inference_enabled: false },
			security: {
				permission_mode: 'default',
				sandbox_mode: 'workspace_write',
				network_policy: 'ask',
				writable_roots: [],
				permissions: [],
			},
			media: {},
			notification: {
				session_created: { in_app: true, windows: false },
				session_completed: { in_app: true, windows: true },
				session_paused: { in_app: true, windows: false },
				session_resumed: { in_app: true, windows: false },
				session_error: { in_app: true, windows: true },
				permission_requested: { in_app: true, windows: true },
				tool_run_completed: { in_app: true, windows: true },
			},
			log: { level: 'info', file_enabled: true, file_path: 'C:\\Haven\\logs\\custom.log' },
			mcp_servers: [],
		};
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					return settings;
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

		const { container } = render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('is_autostart_enabled'));
		await fireEvent.click(screen.getByRole('tab', { name: /对话与行为/ }));
		expect(((await screen.findByLabelText('会话累计步骤上限')) as HTMLInputElement).value).toBe(
			'750',
		);
		await fireEvent.click(screen.getByRole('tab', { name: /语音与媒体/ }));
		expect(
			((await screen.findByLabelText('托管上传总量上限（MiB）')) as HTMLInputElement).value,
		).toBe('512');
		await fireEvent.click(screen.getByRole('tab', { name: /性能与限制/ }));
		expect(((await screen.findByLabelText('事实提取最短间隔')) as HTMLInputElement).value).toBe(
			'77',
		);
		await fireEvent.click(screen.getByRole('tab', { name: /界面与通知/ }));
		await fireEvent.click(await screen.findByRole('switch', { name: '会话开始 Windows 通知' }));
		expect(screen.getByRole('tab', { name: /界面与通知.*已修改/ })).toBeTruthy();
		expect(
			(container.querySelector('#session-lifetime-max-steps') as HTMLInputElement).value,
		).toBe('750');
		await fireEvent.click(await screen.findByRole('button', { name: '保存' }));

		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('update_settings', expect.anything()),
		);
		const update = invoke.mock.calls.find(([command]) => command === 'update_settings');
		const saved = update?.[1]?.settings;
		expect(saved.session.session_max_steps).toBe(750);
		expect(saved.memory.fact_inference_enabled).toBe(false);
		expect(saved.hotkey.mute_hotkey).toBe('Ctrl+M');
		expect(saved.log.file_path).toBe('C:\\Haven\\logs\\custom.log');
		expect(saved.context_limits).toMatchObject({
			tool_run_result_context_chars: 4321,
			fact_extraction_min_interval_secs: 77,
			turn_deadline_secs: 901,
		});
	}, 15_000);

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
		await fireEvent.click(screen.getByRole('tab', { name: /模型与连接/ }));
		await fireEvent.click(await screen.findByRole('button', { name: '添加 Provider' }));
		await fireEvent.input(screen.getByPlaceholderText('唯一名称，角色据此选择'), {
			target: { value: 'primary' },
		});
		await fireEvent.input(screen.getByPlaceholderText('sk-...'), {
			target: { value: 'provider-secret-marker' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));
		await fireEvent.click(screen.getByRole('tab', { name: /对话与行为/ }));
		expect(screen.queryByRole('dialog', { name: '未保存的更改' })).toBeNull();
		await fireEvent.click(screen.getByRole('tab', { name: /模型与连接/ }));
		await fireEvent.click(await screen.findByRole('button', { name: '保存' }));

		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('update_settings', expect.anything()),
		);
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
