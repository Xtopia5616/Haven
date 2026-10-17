import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SettingsView from './SettingsView.svelte';
import { createSettingsFixture } from '$lib/contracts/settingsFixture.ts';
import { AGENT_METRICS_PHASE_COUNT } from '$lib/contracts/generatedCommands.ts';

const { invoke, listen } = vi.hoisted(() => ({
	invoke: vi.fn(),
	listen: vi.fn(),
}));

import { onNotificationSettingsChanged } from '$lib/notificationSettingsEvents.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke, listen }));

describe('SettingsView diagnostics export', () => {
	beforeEach(() => {
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					return null;
				case 'get_api_key_status':
					return { models: {}, providers: {}, stt: false, ocr: false, ocr_secret: false };
				case 'is_autostart_enabled':
					return false;
				case 'get_performance_metrics':
					return {
						phases: Array.from({ length: AGENT_METRICS_PHASE_COUNT }, () => ({
							count: 0,
							total_ms: 0,
							p50_ms: 0,
							p95_ms: 0,
						})),
						counters: {
							turn_starts: 0,
							first_tokens: 0,
							stream_chunks: 0,
							chunk_drops: 0,
							checkpoint_pending: 0,
							branch_point_failures: 0,
							event_boundary_check_failures: 0,
							projection_failures: 0,
							inbox_ack_failures: 0,
							tool_run_result_retries: 0,
							tool_run_result_duplicates: 0,
							web_search_drops: 0,
							requests: 1,
						},
						gauges: { context_queue_items: 0 },
						ui: { frames: 2, chunks: 0, drops: 0 },
					};
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
		const settings = createSettingsFixture();
		settings.llm.providers = [
			{
				name: 'primary',
				provider: 'openai',
				api_style: 'openai-chat',
				base_url: 'https://example.test/v1',
				auth_header_name: 'Authorization',
				auth_header_prefix: 'Bearer',
				proxy_url: null,
				no_proxy: null,
			},
		];
		settings.llm.models = [
			{
				id: 'chat-slot',
				provider_name: 'primary',
				model: 'before-switch',
				capabilities: ['chat'],
			},
		];
		settings.llm.request_policies = [{ request: 'chat', primary: 'chat-slot' }];
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
		const settings = createSettingsFixture();
		settings.llm.providers = [
			{
				name: 'primary',
				provider: 'openai',
				api_style: 'openai-chat',
				base_url: 'https://example.test/v1',
				auth_header_name: 'Authorization',
				auth_header_prefix: 'Bearer',
				proxy_url: null,
				no_proxy: null,
			},
		];
		settings.llm.models = [
			{
				id: 'chat-slot',
				provider_name: 'primary',
				model: 'gpt-test',
				capabilities: ['chat'],
			},
		];
		settings.llm.request_policies = [];
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
		expect(container.querySelector('[role="table"]')).toBeNull();

		settings.llm.request_policies = [{ request: 'chat', primary: 'chat-slot' }];
		handlers.get('llm:config_changed')?.({
			event: 'llm:config_changed',
			id: 1,
			payload: null,
		});

		await waitFor(() =>
			expect(container.querySelector('.policy-table-row')?.textContent).toContain('chat'),
		);
		expect(screen.getByRole('table', { name: '请求路由策略' })).toBeDefined();
		expect(screen.getByRole('columnheader', { name: '模型配置 ID' })).toBeDefined();
		expect(screen.getByRole('button', { name: 'chat 的模型配置 ID' }).textContent).toContain(
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

	it.each(['null', 'reject', 'incomplete'] as const)(
		'does not allow a full Settings save when get_settings %s',
		async (failureKind) => {
			invoke.mockImplementation(async (command: string) => {
				switch (command) {
					case 'get_settings':
						if (failureKind === 'reject') throw new Error('settings unavailable');
						if (failureKind === 'incomplete') return { llm: { providers: [] } };
						return null;
					case 'get_api_key_status':
						return {
							models: {},
							providers: {},
							stt: false,
							ocr: false,
							ocr_secret: false,
						};
					case 'is_autostart_enabled':
						return false;
					default:
						return [];
				}
			});

			render(SettingsView);
			await waitFor(() => expect(invoke).toHaveBeenCalledWith('is_autostart_enabled'));
			await fireEvent.click(screen.getByRole('tab', { name: /界面与通知/ }));
			await fireEvent.click(
				await screen.findByRole('switch', { name: '会话开始 Windows 通知' }),
			);

			expect(screen.queryByRole('button', { name: '保存' })).toBeNull();
			expect(invoke).not.toHaveBeenCalledWith('update_settings', expect.anything());
		},
	);

	it('persists ToolRun completion channels independently and applies the toast switch', async () => {
		const settings = createSettingsFixture();
		settings.notification.tool_run_completed.in_app = true;
		settings.notification.tool_run_completed.windows = true;
		settings.context_limits.scheduled_tool_runs_due_horizon_secs = 365 * 24 * 3600;
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
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('is_autostart_enabled'));
		await new Promise((resolve) => setTimeout(resolve, 0));
		await fireEvent.click(screen.getByRole('tab', { name: /界面与通知/ }));

		const inApp = await screen.findByRole('switch', { name: '任务完成应用内提示' });
		const windows = screen.getByRole('switch', { name: '任务完成 Windows 通知' });
		expect((inApp as HTMLInputElement).checked).toBe(true);
		expect((windows as HTMLInputElement).checked).toBe(true);

		const publishedNotification: {
			value: import('$lib/contracts/generatedCommands.ts').NotificationConfigInput | null;
		} = { value: null };
		const unsubscribe = onNotificationSettingsChanged((notification) => {
			publishedNotification.value = notification;
		});
		await fireEvent.click(inApp);
		await waitFor(() => expect(screen.getByRole('button', { name: '保存' })).toBeTruthy());
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
		expect(publishedNotification.value?.tool_run_completed).toEqual({
			in_app: false,
			windows: false,
		});
		unsubscribe();
		const update = invoke.mock.calls.find(([command]) => command === 'update_settings');
		expect(update?.[1]?.settings.context_limits.scheduled_tool_runs_due_horizon_secs).toBe(
			365 * 24 * 3600,
		);
	});

	it('publishes durable-first notification settings after a partial runtime apply failure', async () => {
		const settings = createSettingsFixture();
		settings.notification.tool_run_completed.in_app = true;
		let settingsReads = 0;
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					settingsReads += 1;
					if (settingsReads > 1) {
						return {
							...settings,
							notification: {
								...settings.notification,
								tool_run_completed: { in_app: false, windows: true },
							},
						};
					}
					return settings;
				case 'update_settings':
					throw new Error('部分 apply 失败：config_version=2');
				case 'get_api_key_status':
					return { models: {}, providers: {}, stt: false, ocr: false, ocr_secret: false };
				case 'is_autostart_enabled':
					return false;
				default:
					return [];
			}
		});
		const publishedNotification: {
			value: import('$lib/contracts/generatedCommands.ts').NotificationConfigInput | null;
		} = { value: null };
		const unsubscribe = onNotificationSettingsChanged((notification) => {
			publishedNotification.value = notification;
		});
		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('is_autostart_enabled'));
		await new Promise((resolve) => setTimeout(resolve, 0));
		await fireEvent.click(screen.getByRole('tab', { name: /界面与通知/ }));
		const inApp = await screen.findByRole('switch', { name: '任务完成应用内提示' });
		expect((inApp as HTMLInputElement).checked).toBe(true);
		await fireEvent.click(inApp);
		expect((inApp as HTMLInputElement).checked).toBe(false);
		await waitFor(() => expect(screen.getByRole('button', { name: '保存' })).toBeTruthy());
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));

		await waitFor(() => {
			expect(publishedNotification.value?.tool_run_completed).toEqual({
				in_app: false,
				windows: true,
			});
		});
		unsubscribe();
	});

	it('does not publish notification draft values when persistence fails', async () => {
		const settings = createSettingsFixture();
		settings.notification.tool_run_completed.in_app = true;
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					return settings;
				case 'update_settings':
					throw new Error('disk write failed');
				case 'get_api_key_status':
					return { models: {}, providers: {}, stt: false, ocr: false, ocr_secret: false };
				case 'is_autostart_enabled':
					return false;
				default:
					return [];
			}
		});
		const publishedNotification: {
			value: import('$lib/contracts/generatedCommands.ts').NotificationConfigInput | null;
		} = { value: null };
		const unsubscribe = onNotificationSettingsChanged((notification) => {
			publishedNotification.value = notification;
		});
		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('is_autostart_enabled'));
		await new Promise((resolve) => setTimeout(resolve, 0));
		await fireEvent.click(screen.getByRole('tab', { name: /界面与通知/ }));
		const inApp = await screen.findByRole('switch', { name: '任务完成应用内提示' });
		expect((inApp as HTMLInputElement).checked).toBe(true);
		await fireEvent.click(inApp);
		expect((inApp as HTMLInputElement).checked).toBe(false);
		await waitFor(() => expect(screen.getByRole('button', { name: '保存' })).toBeTruthy());
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));

		await waitFor(() => expect(screen.getByText('disk write failed')).toBeTruthy());
		expect(publishedNotification.value).toBeNull();
		unsubscribe();
	});

	it('preserves backend settings that are outside the visible category during a full save', async () => {
		invoke.mockClear();
		const settings = createSettingsFixture();
		settings.default_shell = 'powershell';
		settings.llm.providers = [
			{
				name: 'deepgram-voice',
				provider: 'deepgram',
				api_style: 'deepgram',
				base_url: 'https://api.deepgram.com',
				auth_header_name: 'Authorization',
				auth_header_prefix: 'Token',
				proxy_url: null,
				no_proxy: null,
			},
			{
				name: 'speech-connection',
				provider: 'openai',
				api_style: 'openai-chat',
				base_url: 'https://speech.example/v1',
				auth_header_name: 'Authorization',
				auth_header_prefix: 'Bearer',
				proxy_url: null,
				no_proxy: null,
			},
			{
				name: 'image-connection',
				provider: 'openai',
				api_style: 'openai-chat',
				base_url: 'https://image.example/v1',
				auth_header_name: 'Authorization',
				auth_header_prefix: 'Bearer',
				proxy_url: null,
				no_proxy: null,
			},
		];
		settings.llm.max_concurrent_requests = 2;
		settings.hotkey.mode = 'toggle';
		settings.hotkey.key_binding = 'Ctrl+Shift+Space';
		settings.hotkey.mute_hotkey = 'Ctrl+M';
		settings.session.max_concurrent = 3;
		settings.session.prompt_history_limit = 73;
		settings.session.max_steps_per_run = 500;
		settings.session.history_retention_days = 90;
		settings.session.max_steps_per_session = 750;
		Object.assign(settings.context_limits, {
			compaction_ratio: 0.65,
			skills_max_line_bytes: 2048,
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
			response_token_floor: 64_000,
			file_max_full_file_bytes: 256_000,
			file_line_max_bytes: 32_000,
			file_byte_read_max_bytes: 8 * 1024 * 1024,
			file_write_max_bytes: 32 * 1024 * 1024,
			mcp_max_binary_base64_bytes: 1024 * 1024,
			max_pending_scheduled_tool_runs: 17,
			max_running_background_tool_runs: 29,
		});
		settings.memory.fact_inference_enabled = false;
		settings.security.permission_mode = 'default';
		settings.security.sandbox_mode = 'workspace_write';
		settings.security.network_policy = 'ask';
		settings.security.execution_root = 'C:\\Haven\\Projects\\demo';
		settings.media.stt.provider_name = 'deepgram-voice';
		settings.media.stt.model = 'nova-3';
		settings.media.ocr.provider = 'azure';
		settings.media.tts.provider_name = 'speech-connection';
		settings.media.tts.model = 'tts-1';
		settings.media.image_gen.provider_name = 'image-connection';
		settings.media.image_gen.model = 'gpt-image-1';
		Object.assign(settings.notification.session_created, { in_app: true, windows: false });
		Object.assign(settings.notification.session_completed, { in_app: true, windows: true });
		Object.assign(settings.notification.session_paused, { in_app: true, windows: false });
		Object.assign(settings.notification.session_resumed, { in_app: true, windows: false });
		Object.assign(settings.notification.session_error, { in_app: true, windows: true });
		Object.assign(settings.notification.permission_requested, { in_app: true, windows: true });
		Object.assign(settings.notification.tool_run_completed, { in_app: true, windows: true });
		settings.log.level = 'info';
		settings.log.file_enabled = true;
		settings.log.file_path = 'C:\\Haven\\logs\\custom.log';
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
		expect(((await screen.findByLabelText('会话累计最大步数')) as HTMLInputElement).value).toBe(
			'750',
		);
		expect(
			((await screen.findByLabelText('首次提示的历史消息数')) as HTMLInputElement).value,
		).toBe('73');
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
			(container.querySelector('#session-max-steps-per-session') as HTMLInputElement).value,
		).toBe('750');
		await fireEvent.click(await screen.findByRole('button', { name: '保存' }));

		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('update_settings', expect.anything()),
		);
		const update = invoke.mock.calls.find(([command]) => command === 'update_settings');
		const saved = update?.[1]?.settings;
		expect(saved.session.max_steps_per_session).toBe(750);
		expect(saved.session.prompt_history_limit).toBe(73);
		expect(saved.memory.fact_inference_enabled).toBe(false);
		expect(saved.security.execution_root).toBe('C:\\Haven\\Projects\\demo');
		expect(saved.hotkey.mute_hotkey).toBe('Ctrl+M');
		expect(saved.log.file_path).toBe('C:\\Haven\\logs\\custom.log');
		expect(saved.media.stt.provider_name).toBe('deepgram-voice');
		expect(saved.media.ocr.provider).toBe('azure');
		expect(saved.media.tts.provider_name).toBe('speech-connection');
		expect(saved.media.image_gen.provider_name).toBe('image-connection');
		expect(saved.context_limits).toMatchObject({
			response_token_floor: 64_000,
			skills_max_line_bytes: 2048,
			file_max_full_file_bytes: 256_000,
			file_line_max_bytes: 32_000,
			file_byte_read_max_bytes: 8 * 1024 * 1024,
			file_write_max_bytes: 32 * 1024 * 1024,
			mcp_max_binary_base64_bytes: 1024 * 1024,
			max_pending_scheduled_tool_runs: 17,
			max_running_background_tool_runs: 29,
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
					return createSettingsFixture();
				case 'stage_provider_credential':
					return 'cred-b338124556ee7e51cdde231e1e5a64e6';
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
		expect(settings.llm.providers[0].api_key_ref).toBe('cred-b338124556ee7e51cdde231e1e5a64e6');
		expect(JSON.stringify(settings)).not.toContain('provider-secret-marker');
	});
});
