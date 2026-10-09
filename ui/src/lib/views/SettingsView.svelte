<script lang="ts">
	/**
	 * Owns the single full-Settings draft, save baseline, and leave guard.
	 * The Rust update command accepts one full Settings snapshot, so persistence
	 * and dirty comparison stay centralized while each intent group owns its UI.
	 */
	let {
		isVisible = true,
		entering = false,
		onAnimationEnd = () => {},
	}: {
		isVisible?: boolean;
		entering?: boolean;
		onAnimationEnd?: (event: AnimationEvent) => void;
	} = $props();
	import { onMount, onDestroy, tick } from 'svelte';
	import { registerListeners } from '$lib/events.ts';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialTabs from '$lib/MaterialTabs.svelte';
	import { addNotification } from '$lib/notificationStore.ts';
	import { setToolRunCompletionNotificationChannels } from '$lib/toolRunCompletionNotificationSettings.ts';
	import { formatError } from '$lib/formatError.ts';
	import {
		isPartialConfigApplyError,
		PARTIAL_APPLY_SAVE_MESSAGE,
	} from '$lib/settingsSaveFailure.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { registerSettingsLeaveGuard } from '$lib/settingsGuard.ts';
	import { resolveSettingsSaveAction } from '$lib/settingsSaveAction.ts';
	import {
		disableAutostart,
		discardStagedCredentials,
		enableAutostart,
		isAutostartEnabled,
		listSessionPermissions,
		loadSettings,
		resetPermissions as resetPermissionsCommand,
		resetSessionPermissions as resetSessionPermissionsCommand,
		revokePermission as revokePermissionCommand,
		revokeSessionPermission as revokeSessionPermissionCommand,
		runMemoryMaintenance,
		setHotkeyCaptureActive as setHotkeyCaptureActiveCommand,
		stageOcrCredential,
		stageProviderCredential,
		updateSettings as updateSettingsCommand,
	} from '$lib/settingsCommands.ts';
	import { checkShellAvailable, readApiKeyStatus } from '$lib/diagnosticsCommands.ts';
	import { SHELL_CHOICE_INPUT_VALUES } from '$lib/contracts/generatedCommands.ts';
	import ModelSettings from './ModelSettings.svelte';
	import type { DiscoveredModelMetadataFill } from '$lib/modelDiscovery.ts';
	import SettingsBehavior from './SettingsBehavior.svelte';
	import SettingsAppearance from './SettingsAppearance.svelte';
	import SettingsDiagnostics from './SettingsDiagnostics.svelte';
	import SettingsSecurity from './SettingsSecurity.svelte';
	import SettingsLimits from './SettingsLimits.svelte';
	import SettingsSaveBar from './SettingsSaveBar.svelte';
	import WorkspacePageHeader from '$lib/WorkspacePageHeader.svelte';
	import WorkspaceSurface from '$lib/WorkspaceSurface.svelte';
	import type {
		ApiKeyStatus,
		AudioConfigInput,
		AudioConfig,
		ContextLimitsConfigInput,
		HotkeyModeInput,
		ImageGenConfigInput,
		LogConfigInput,
		MediaInputStrategyInput,
		MemoryConfigInput,
		NotificationConfigInput,
		OcrConfigInput,
		PermissionModeInput,
		SandboxModeInput,
		SecurityConfigInput,
		SessionConfigInput,
		ShellChoiceInput,
		SttConfigInput,
		TtsConfigInput,
	} from '$lib/contracts/generatedCommands.ts';
	import type { SessionTokenStatsView } from '$lib/sessionUsagePresentation.ts';
	import type { SessionLlmUsage } from '$lib/contracts/sessionHistory.ts';
	import type {
		SessionPermissionGrant,
		StoredPermission,
	} from '$lib/contracts/generatedCommands.ts';
	import type { DiscoveredModelMap } from '$lib/contracts/model.ts';
	import {
		settingsLlmInputFromState,
		settingsLlmStateFromConfig,
		modelDraftFromConfig,
		type ModelDraft,
		type ProviderDraft,
		type SettingsLlmState,
	} from '$lib/settingsModelTypes.ts';

	type SettingsSnapshot = {
		default_shell?: ShellChoiceInput;
		llm?: SettingsLlmState;
		hotkey?: { key_binding?: string; mode?: HotkeyModeInput; mute_hotkey?: string | null };
		session?: Partial<
			Required<
				Pick<
					SessionConfigInput,
					| 'max_concurrent'
					| 'max_steps_per_run'
					| 'history_retention_days'
					| 'max_steps_per_session'
					| 'prompt_history_limit'
				>
			>
		>;
		memory?: Partial<Required<Pick<MemoryConfigInput, 'fact_inference_enabled'>>>;
		security?: Omit<SecurityConfigInput, 'permissions'> & { permissions?: StoredPermission[] };
		context_limits?: Partial<ContextLimitsConfigInput>;
		media?: {
			input_strategy?: MediaInputStrategyInput;
			audio?: Partial<AudioConfigInput>;
			stt?: Partial<SttConfigInput>;
			ocr?: Partial<OcrConfigInput>;
			tts?: Partial<TtsConfigInput>;
			image_gen?: Partial<ImageGenConfigInput>;
		};
		notification?: Partial<Required<NotificationConfigInput>>;
		log?: Partial<Required<LogConfigInput>>;
		autostart_enabled?: boolean;
		key_configured?: Partial<ApiKeyStatus>;
		key_configured_providers?: Record<string, boolean>;
	};

	let llmConfig = $state<SettingsLlmState>({
		providers: [],
		models: [],
		request_policies: [],
		max_concurrent_requests: 2,
	});
	let keyConfigured = $state<ApiKeyStatus>({
		models: {},
		providers: {},
		stt: false,
		ocr: false,
		ocr_secret: false,
	});
	let keyConfiguredProviders = $state<Record<string, boolean>>({});
	let hotkeyMode = $state<HotkeyModeInput>('toggle');
	let hotkeyBinding = $state('Ctrl+Shift+Space');
	let muteHotkey = $state<string | null>(null);
	let autostartEnabled = $state(false);
	let defaultShell = $state<ShellChoiceInput>('powershell');
	let shellAvailable = $state<Record<ShellChoiceInput, boolean>>({
		cmd: false,
		powershell: false,
		pwsh: false,
	});
	let audio = $state<AudioConfig>({
		sample_rate: 16000,
		channels: 1,
		bits_per_sample: 16,
		max_duration_secs: 60,
		silence_timeout_ms: 1500,
		vad_threshold: 0.5,
	});
	let session = $state<
		Required<
			Pick<
				SessionConfigInput,
				| 'max_concurrent'
				| 'max_steps_per_run'
				| 'history_retention_days'
				| 'max_steps_per_session'
				| 'prompt_history_limit'
			>
		>
	>({
		max_concurrent: 3,
		prompt_history_limit: 50,
		max_steps_per_run: 500,
		history_retention_days: 90,
		max_steps_per_session: null,
	});
	let contextLimits = $state<Partial<ContextLimitsConfigInput>>({
		compaction_ratio: 0.65,
		compaction_reserve_tokens: 8192,
		default_context_window: 64000,
		max_response_tokens: 32000,
		max_observation_chars: 16000,
		max_transcript_chars: 4000,
		max_attachment_images: 4,
		max_attachment_files: 5,
		max_attachment_image_bytes: 10 * 1024 * 1024,
		max_attachment_file_bytes: 20 * 1024 * 1024,
		max_upload_total_bytes: 512 * 1024 * 1024,
		max_attachment_image_dim_px: 1568,
		attachment_image_jpeg_quality: 0.85,
		file_read_max_chars: 128000,
		file_line_span: 100,
		file_max_line_chars: 128000,
		file_summary_input_chars: 60000,
		file_max_list_entries: 1000,
		file_max_byte_read: 16 * 1024 * 1024,
		file_vision_max_bytes: 8 * 1024 * 1024,
		search_snippet_chars: 640,
		search_max_results: 1000,
		search_max_file_size_bytes: 100 * 1024 * 1024,
		search_window_bytes: 16 * 1024 * 1024,
		notification_summary_chars: 800,
		partial_checkpoint_min_chars: 1000,
		partial_checkpoint_interval_secs: 2,
		fact_infer_interval_steps: 25,
		fact_extraction_min_interval_secs: 60,
		max_known_facts: 40,
		sanitize_field_max_chars: 256,
		file_summary_timeout_secs: 120,
		tool_run_result_context_chars: 4000,
		turn_deadline_secs: 300,
		incomplete_tool_args_retries: 2,
		stream_stall_warn_delay_ms: 10000,
		reasoning_echo_max_chars: 1200,
		background_tool_run_tail_max_chars: 2000,
		background_tool_run_output_emit_interval_ms: 1500,
		tool_run_terminal_ttl_secs: 600,
		mcp_max_binary_payload_bytes: 2 * 1024 * 1024,
		mcp_max_sse_buffer_bytes: 2 * 1024 * 1024,
		skills_max_md_bytes: 256 * 1024,
		skills_max_parse_lines: 5000,
		skills_max_line_len: 4096,
		self_tool_max_instructions_bytes: 256 * 1024,
		self_tool_max_script_bytes: 512 * 1024,
		network_max_retries: 2,
		network_backoff_base_secs: 1,
		network_max_body_bytes: 1024 * 1024,
		clipboard_history_entries: 10,
		clipboard_history_max_entries: 100,
		clipboard_entry_max_chars: 2000,
		scheduled_tool_runs_max: 32,
		background_max_tool_runs: 64,
		event_chunk_batch_max_bytes: 8 * 1024,
		input_ring_buffer_secs: 20,
		embedding_chunk_size: 10,
		max_tools_per_request: 256,
	});
	let memory = $state<Required<Pick<MemoryConfigInput, 'fact_inference_enabled'>>>({
		fact_inference_enabled: true,
	});
	let memoryMaintenance = $state<{ running: boolean; lastCount: number | null }>({
		running: false,
		lastCount: null,
	});
	let security = $state<SecurityConfigInput & { permissions: StoredPermission[] }>({
		permission_mode: 'default',
		sandbox_mode: 'workspace_write',
		network_policy: 'ask',
		writable_roots: [],
		permissions: [],
	});
	let sessionPermissions = $state<SessionPermissionGrant[]>([]);
	let stt = $state<Required<SttConfigInput>>({
		provider: 'llm',
		mcp_server: '',
		model: '',
		timeout_secs: 30,
		min_confidence: 0.7,
	});
	let ocr = $state<Required<OcrConfigInput>>({
		provider: 'llm',
		api_key: '',
		api_key_ref: null,
		api_secret: '',
		api_secret_ref: null,
		base_url: '',
		timeout_secs: 20,
		min_confidence: 0.7,
	});
	let tts = $state<Required<TtsConfigInput>>({
		provider: 'none',
		model: '',
		voice: '',
		timeout_secs: 60,
	});
	let imageGen = $state<Required<ImageGenConfigInput>>({
		provider: 'none',
		model: '',
		timeout_secs: 120,
	});
	let mediaInputStrategy = $state<MediaInputStrategyInput>('auto');
	let notification = $state<Required<NotificationConfigInput>>({
		session_created: { in_app: true, windows: false },
		session_completed: { in_app: true, windows: true },
		session_paused: { in_app: true, windows: false },
		session_resumed: { in_app: true, windows: false },
		session_error: { in_app: true, windows: true },
		permission_requested: { in_app: true, windows: true },
		tool_run_completed: { in_app: true, windows: true },
	});
	let log = $state<Required<LogConfigInput>>({
		level: 'info',
		file_enabled: true,
		file_path: null,
	});

	const SETTINGS_SECTIONS = [
		{
			id: 'behavior',
			label: '对话与行为',
			description: '配置语音快捷键、会话执行、命令行工具和记忆。',
			keys: ['hotkey', 'session', 'memory', 'default_shell'],
		},
		{
			id: 'models',
			label: '模型与连接',
			description: '管理 Provider、模型目录、请求能力和模型路由。',
			keys: ['llm'],
		},
		{
			id: 'media',
			label: '语音与媒体',
			description: '配置录音、转写、OCR、语音合成和图像生成。',
			keys: ['media'],
		},
		{
			id: 'appearance',
			label: '界面与通知',
			description: '调整显示风格、事件通知和 Windows 启动行为。',
			keys: ['notification', 'autostart_enabled'],
		},
		{
			id: 'security',
			label: '安全与权限',
			description: '设置授权方式、文件沙箱、网络策略和永久规则。',
			keys: ['security'],
		},
		{
			id: 'limits',
			label: '性能与限制',
			description: '调整上下文、工具、文件、并发和资源保护上限。',
			keys: ['context_limits'],
		},
		{
			id: 'diagnostics',
			label: '日志与诊断',
			description: '管理后端日志并导出性能诊断数据。',
			keys: ['log'],
		},
	] as const;
	type SettingsTabId = (typeof SETTINGS_SECTIONS)[number]['id'];
	let settingsTab = $state<SettingsTabId>('behavior');
	let visitedSettingsTabs = $state<SettingsTabId[]>(['behavior']);
	let modelSection = $state<Extract<SettingsTabId, 'models' | 'media'>>('models');
	let activeSettingsSection = $derived(
		SETTINGS_SECTIONS.find((section) => section.id === settingsTab) ?? SETTINGS_SECTIONS[0],
	);
	let dirtySettingsSectionIds = $derived.by(() => {
		if (!settingsLoaded || !savedSnapshot) return [] as SettingsTabId[];
		try {
			const current = buildPersistableSettings() as Record<string, unknown>;
			const baseline = JSON.parse(savedSnapshot) as Record<string, unknown>;
			return SETTINGS_SECTIONS.filter((section) =>
				section.keys.some(
					(key) => JSON.stringify(current[key]) !== JSON.stringify(baseline[key]),
				),
			).map((section) => section.id);
		} catch {
			return [] as SettingsTabId[];
		}
	});
	let settingsTabs = $derived(
		SETTINGS_SECTIONS.map((section) => ({
			id: section.id,
			label: section.label,
			hint: dirtySettingsSectionIds.includes(section.id) ? '已修改' : undefined,
		})),
	);
	let dirtySettingsSectionLabels = $derived(
		SETTINGS_SECTIONS.filter((section) => dirtySettingsSectionIds.includes(section.id)).map(
			(section) => section.label,
		),
	);
	let providerDiscoveryAlert = $state<{ providerName: string; staticCatalog: boolean }>({
		providerName: '',
		staticCatalog: false,
	});
	let mcpServerNames = $state<string[]>([]);
	let settingsLoaded = $state(false);
	let savedSnapshot = $state('');
	let leaveDialogOpen = $state(false);
	let leaveDialogResolve: ((ok: boolean) => void) | null = null;
	let leaveSaving = $state(false);
	let saveState = $state<'idle' | 'saving' | 'saved' | 'error'>('idle');
	let saveError = $state('');
	let saveBarHeight = $state(0);
	let settingsViewElement = $state<HTMLDivElement | null>(null);
	let securityRuntimeStatus = $state<'current' | 'unchanged' | 'incomplete'>('current');
	let securityRuntimeNotice = $state('安全策略已按当前配置完成运行时应用。');
	let mounted = true;
	let eventRegistrations: ReturnType<typeof registerListeners> | null = null;
	let chatModelSyncGen = 0;
	let skipNextChatModelSync = false;

	async function checkShells() {
		for (const shell of SHELL_CHOICE_INPUT_VALUES) {
			try {
				shellAvailable[shell] = (await checkShellAvailable({ shell })).available;
			} catch (error) {
				shellAvailable[shell] = false;
				reportError(error, {
					context: 'SettingsView',
					message: `检测 ${shell} 可用性失败`,
					notify: false,
				});
			}
		}
	}

	function runtimeModelFieldValue(
		model: Pick<ModelDraft, 'model' | 'reasoning_effort' | 'web_search'>,
		field: 'model' | 'reasoning_effort' | 'web_search',
	) {
		if (field === 'web_search') return model.web_search || 'off';
		return model[field] || '';
	}
	function copyRuntimeModelField(
		target: ModelDraft,
		source: ModelDraft,
		field: 'model' | 'reasoning_effort' | 'web_search',
	) {
		if (field === 'model') target.model = source.model;
		else if (field === 'reasoning_effort') target.reasoning_effort = source.reasoning_effort;
		else target.web_search = source.web_search;
	}
	function asNumber(value: unknown) {
		const number = Number(value);
		return Number.isFinite(number) ? number : 0;
	}

	function buildPersistableSettings() {
		return {
			default_shell: defaultShell,
			llm: llmConfig,
			hotkey: { key_binding: hotkeyBinding, mode: hotkeyMode, mute_hotkey: muteHotkey },
			session: {
				max_concurrent: asNumber(session.max_concurrent),
				prompt_history_limit: asNumber(session.prompt_history_limit),
				max_steps_per_run: asNumber(session.max_steps_per_run),
				history_retention_days: asNumber(session.history_retention_days),
				max_steps_per_session: session.max_steps_per_session ?? null,
			},
			memory: {
				fact_inference_enabled: memory.fact_inference_enabled,
			},
			security: {
				permission_mode: security.permission_mode,
				sandbox_mode: security.sandbox_mode,
				network_policy: security.network_policy,
				writable_roots: security.writable_roots,
				permissions: security.permissions,
			},
			context_limits: contextLimits,
			media: {
				input_strategy: mediaInputStrategy,
				audio: {
					sample_rate: asNumber(audio.sample_rate),
					channels: asNumber(audio.channels),
					bits_per_sample: asNumber(audio.bits_per_sample),
					max_duration_secs: asNumber(audio.max_duration_secs),
					silence_timeout_ms: asNumber(audio.silence_timeout_ms),
					vad_threshold: asNumber(audio.vad_threshold),
				},
				stt: {
					provider: stt.provider,
					mcp_server: stt.mcp_server || null,
					model: stt.model,
					timeout_secs: asNumber(stt.timeout_secs),
					min_confidence: asNumber(stt.min_confidence),
				},
				ocr: {
					...ocr,
					timeout_secs: asNumber(ocr.timeout_secs),
					min_confidence: asNumber(ocr.min_confidence),
				},
				tts: {
					provider: tts.provider,
					model: tts.model,
					voice: tts.voice,
					timeout_secs: asNumber(tts.timeout_secs),
				},
				image_gen: {
					provider: imageGen.provider,
					model: imageGen.model,
					timeout_secs: asNumber(imageGen.timeout_secs),
				},
			},
			notification: {
				session_created: { ...notification.session_created },
				session_completed: { ...notification.session_completed },
				session_paused: { ...notification.session_paused },
				session_resumed: { ...notification.session_resumed },
				session_error: { ...notification.session_error },
				permission_requested: { ...notification.permission_requested },
				tool_run_completed: { ...notification.tool_run_completed },
			},
			log: {
				level: log.level,
				file_enabled: log.file_enabled,
				file_path: log.file_path ?? null,
			},
			autostart_enabled: autostartEnabled,
			key_configured: { ...keyConfigured },
			key_configured_providers: { ...keyConfiguredProviders },
		};
	}
	function captureSnapshot() {
		savedSnapshot = JSON.stringify(buildPersistableSettings());
	}
	function isDirty() {
		return (
			settingsLoaded &&
			!!savedSnapshot &&
			JSON.stringify(buildPersistableSettings()) !== savedSnapshot
		);
	}
	const settingsDirty = $derived.by(() => isDirty());
	const saveBarVisible = $derived(settingsDirty || saveState === 'error');

	function discardAndReset() {
		discardChanges();
		saveState = 'idle';
		saveError = '';
	}

	function isSettingsTabId(value: string): value is SettingsTabId {
		return SETTINGS_SECTIONS.some((section) => section.id === value);
	}

	function changeSettingsTab(id: string) {
		if (!isSettingsTabId(id) || id === settingsTab) return;
		if (id === 'models' || id === 'media') modelSection = id;
		if (!visitedSettingsTabs.includes(id)) visitedSettingsTabs = [...visitedSettingsTabs, id];
		settingsTab = id;
	}

	function reBaselineAfterDiscovery(fills: DiscoveredModelMetadataFill[]) {
		if (!mounted || !settingsLoaded || !savedSnapshot || fills.length === 0) return;
		try {
			const snapshot = JSON.parse(savedSnapshot) as SettingsSnapshot;
			const llmSnapshot = snapshot.llm ?? llmConfig;
			const models = llmSnapshot.models;
			for (const fill of fills) {
				const model = models.find((item) => item.id === fill.id);
				if (!model) continue;
				if (fill.context_window !== undefined) model.context_window = fill.context_window;
				if (fill.cost_per_1k_input_tokens !== undefined)
					model.cost_per_1k_input_tokens = fill.cost_per_1k_input_tokens;
				if (fill.cost_per_1k_output_tokens !== undefined)
					model.cost_per_1k_output_tokens = fill.cost_per_1k_output_tokens;
			}
			snapshot.llm = { ...llmSnapshot, models };
			savedSnapshot = JSON.stringify(snapshot);
		} catch (error) {
			reportError(error, {
				context: 'SettingsView',
				message: '更新模型发现基线失败',
				notify: false,
			});
		}
	}

	/**
	 * Permission rules have an immediate command lifecycle, while the rest of
	 * the settings form is saved in one batch. Update only the permission part
	 * of the baseline so unrelated unsaved edits remain dirty and discard does
	 * not resurrect a rule that was already revoked on disk.
	 */
	function patchSnapshotSecurityPermissions(permissions: StoredPermission[]) {
		if (!savedSnapshot) return;
		try {
			const snapshot = JSON.parse(savedSnapshot) as SettingsSnapshot;
			snapshot.security = {
				...snapshot.security,
				permissions: Array.isArray(permissions) ? permissions : [],
			};
			savedSnapshot = JSON.stringify(snapshot);
		} catch (error) {
			reportError(error, {
				context: 'SettingsView',
				message: '更新权限快照失败',
				notify: false,
			});
		}
	}

	function applyRemoteChatModelFields(remote: ModelDraft) {
		const local = llmConfig.models.find((model) => model.id === remote.id);
		const fields = ['model', 'reasoning_effort', 'web_search'] as const;
		let snapshot: SettingsSnapshot | null = null;
		let snapshotModel: ModelDraft | undefined;
		if (savedSnapshot) {
			try {
				snapshot = JSON.parse(savedSnapshot) as SettingsSnapshot;
				snapshotModel = snapshot.llm?.models.find((model) => model.id === remote.id);
			} catch (error) {
				reportError(error, {
					context: 'SettingsView',
					message: '读取聊天模型设置快照失败',
					notify: false,
				});
			}
		}

		if (local) {
			for (const field of fields) {
				const hasLocalEdit =
					snapshotModel &&
					runtimeModelFieldValue(local, field) !==
						runtimeModelFieldValue(snapshotModel, field);
				if (!hasLocalEdit) copyRuntimeModelField(local, remote, field);
				if (snapshotModel) copyRuntimeModelField(snapshotModel, remote, field);
			}
		} else {
			llmConfig.models.push({ ...remote });
			if (snapshot?.llm) snapshot.llm.models.push({ ...remote });
		}
		if (snapshot) savedSnapshot = JSON.stringify(snapshot);
	}

	function replaceChatPolicy(
		policies: SettingsLlmState['request_policies'],
		remotePolicy: SettingsLlmState['request_policies'][number] | undefined,
	) {
		const chatIndex = policies.findIndex((policy) => policy.request === 'chat');
		const next = policies.filter((policy) => policy.request !== 'chat');
		if (!remotePolicy) return next;
		const insertionIndex = chatIndex < 0 ? next.length : Math.min(chatIndex, next.length);
		next.splice(insertionIndex, 0, { ...remotePolicy });
		return next;
	}

	/**
	 * Chat model switches also mutate request_policies.chat.primary. Keep that
	 * external route change visible in the settings draft and its baseline, while
	 * preserving an unsaved local edit to the same policy.
	 */
	function applyRemoteChatPolicy(remotePolicies: SettingsLlmState['request_policies']) {
		const remotePolicy = remotePolicies.find((policy) => policy.request === 'chat');
		let snapshot: SettingsSnapshot | null = null;
		if (savedSnapshot) {
			try {
				snapshot = JSON.parse(savedSnapshot) as SettingsSnapshot;
			} catch (error) {
				reportError(error, {
					context: 'SettingsView',
					message: '读取对话路由设置快照失败',
					notify: false,
				});
			}
		}

		const localPolicy = llmConfig.request_policies.find((policy) => policy.request === 'chat');
		const baselinePolicy = snapshot?.llm?.request_policies?.find(
			(policy) => policy.request === 'chat',
		);
		const hasLocalChatPolicyEdit =
			(localPolicy?.primary ?? null) !== (baselinePolicy?.primary ?? null);
		if (!hasLocalChatPolicyEdit) {
			llmConfig.request_policies = replaceChatPolicy(
				llmConfig.request_policies,
				remotePolicy,
			);
		}

		if (snapshot?.llm) {
			snapshot.llm.request_policies = replaceChatPolicy(
				snapshot.llm.request_policies || [],
				remotePolicy,
			);
			savedSnapshot = JSON.stringify(snapshot);
		}
	}

	async function syncChatModelFromBackend() {
		const generation = ++chatModelSyncGen;
		try {
			const settings = await loadSettings();
			if (!mounted || generation !== chatModelSyncGen || !settings?.llm) return;
			applyRemoteChatPolicy(settings.llm.request_policies || []);
			const chatPolicy = settings.llm.request_policies.find(
				(policy) => policy.request === 'chat',
			);
			const chatModelId = chatPolicy?.primary || 'default_model';
			const remote = settings.llm.models.find((model) => model.id === chatModelId);
			if (remote) applyRemoteChatModelFields(modelDraftFromConfig(remote));
		} catch (error) {
			reportError(error, {
				context: 'SettingsView',
				message: '同步聊天模型设置失败',
				notify: false,
			});
		}
	}

	async function reconcileChatModelBeforeSave() {
		try {
			const settings = await loadSettings();
			if (!mounted || !settings?.llm) return;
			const chatPolicy = settings.llm.request_policies.find(
				(policy) => policy.request === 'chat',
			);
			const chatModelId = chatPolicy?.primary || 'default_model';
			const remote = settings.llm.models.find((model) => model.id === chatModelId);
			if (remote) applyRemoteChatModelFields(modelDraftFromConfig(remote));
		} catch (error) {
			reportError(error, {
				context: 'SettingsView',
				message: '保存前同步聊天模型设置失败',
				notify: false,
			});
		}
	}

	function discardChanges() {
		requestDiscardStagedCredentials();
		if (!savedSnapshot) return;
		try {
			const snapshot = JSON.parse(savedSnapshot) as SettingsSnapshot;
			defaultShell = snapshot.default_shell || defaultShell;
			if (snapshot.hotkey?.mute_hotkey !== undefined)
				muteHotkey = snapshot.hotkey.mute_hotkey ?? null;
			if (snapshot.llm) {
				llmConfig = {
					...llmConfig,
					...snapshot.llm,
					providers: Array.isArray(snapshot.llm.providers) ? snapshot.llm.providers : [],
					models: Array.isArray(snapshot.llm.models) ? snapshot.llm.models : [],
				};
			}
			if (snapshot.hotkey) {
				hotkeyBinding = snapshot.hotkey.key_binding || hotkeyBinding;
				hotkeyMode = snapshot.hotkey.mode || hotkeyMode;
			}
			if (snapshot.session) session = { ...session, ...snapshot.session };
			if (snapshot.memory) memory = { ...memory, ...snapshot.memory };
			if (snapshot.security)
				security = {
					...security,
					...snapshot.security,
					permissions: Array.isArray(snapshot.security.permissions)
						? snapshot.security.permissions
						: security.permissions,
				};
			if (snapshot.context_limits)
				contextLimits = { ...contextLimits, ...snapshot.context_limits };
			if (snapshot.media?.audio) audio = { ...audio, ...snapshot.media.audio };
			if (snapshot.media?.input_strategy) mediaInputStrategy = snapshot.media.input_strategy;
			if (snapshot.media?.stt)
				stt = {
					provider: snapshot.media.stt.provider || 'llm',
					mcp_server: snapshot.media.stt.mcp_server || '',
					model: snapshot.media.stt.model || '',
					timeout_secs: snapshot.media.stt.timeout_secs || 30,
					min_confidence: snapshot.media.stt.min_confidence ?? 0.7,
				};
			if (snapshot.media?.ocr) ocr = { ...ocr, ...snapshot.media.ocr };
			if (snapshot.media?.tts)
				tts = {
					provider: snapshot.media.tts.provider || 'none',
					model: snapshot.media.tts.model || '',
					voice: snapshot.media.tts.voice || '',
					timeout_secs: snapshot.media.tts.timeout_secs || 60,
				};
			if (snapshot.media?.image_gen)
				imageGen = {
					provider: snapshot.media.image_gen.provider || 'none',
					model: snapshot.media.image_gen.model || '',
					timeout_secs: snapshot.media.image_gen.timeout_secs || 120,
				};
			if (snapshot.notification) notification = { ...notification, ...snapshot.notification };
			if (snapshot.log) log = { ...log, ...snapshot.log };
			if (typeof snapshot.autostart_enabled === 'boolean')
				autostartEnabled = snapshot.autostart_enabled;
			if (snapshot.key_configured)
				keyConfigured = { ...keyConfigured, ...snapshot.key_configured };
			if (snapshot.key_configured_providers)
				keyConfiguredProviders = { ...snapshot.key_configured_providers };
			captureSnapshot();
		} catch (error) {
			reportError(error, {
				context: 'SettingsView',
				message: '恢复设置失败',
				notify: false,
			});
		}
	}

	function requestDiscardStagedCredentials() {
		void discardStagedCredentials().catch((error) =>
			reportError(error, {
				context: 'SettingsView',
				message: '清理未保存的凭据失败',
				log: false,
			}),
		);
	}

	function confirmLeave(): Promise<boolean> {
		if (leaveDialogOpen)
			return new Promise<boolean>((resolve) => {
				const previous = leaveDialogResolve;
				leaveDialogResolve = (ok) => {
					previous?.(false);
					resolve(ok);
				};
			});
		leaveDialogOpen = true;
		return new Promise<boolean>((resolve) => {
			leaveDialogResolve = resolve;
		});
	}
	function finishLeaveDialog(ok: boolean) {
		leaveDialogOpen = false;
		leaveSaving = false;
		const resolve = leaveDialogResolve;
		leaveDialogResolve = null;
		resolve?.(ok);
	}
	function leaveWithoutSaving() {
		discardChanges();
		finishLeaveDialog(true);
	}
	async function leaveWithSaving() {
		leaveSaving = true;
		if (await saveSettings()) finishLeaveDialog(true);
		else leaveSaving = false;
	}
	function stayOnSettings() {
		if (!leaveSaving) finishLeaveDialog(false);
	}

	onDestroy(() => {
		mounted = false;
		requestDiscardStagedCredentials();
		eventRegistrations?.dispose();
		eventRegistrations = null;
		registerSettingsLeaveGuard(null);
		if (leaveDialogResolve) {
			leaveDialogResolve(false);
			leaveDialogResolve = null;
		}
	});

	onMount(async () => {
		registerSettingsLeaveGuard({ isDirty, confirmLeave });
		eventRegistrations = registerListeners(
			{
				'llm:config_changed': () => {
					if (skipNextChatModelSync) {
						skipNextChatModelSync = false;
						return;
					}
					syncChatModelFromBackend();
				},
			},
			{ tag: 'SettingsView' },
		);
		try {
			const settings = await loadSettings();
			if (!mounted) return;
			if (settings) {
				llmConfig = settings.llm ? settingsLlmStateFromConfig(settings.llm) : llmConfig;
				llmConfig.providers = Array.isArray(llmConfig.providers) ? llmConfig.providers : [];
				llmConfig.models = Array.isArray(llmConfig.models) ? llmConfig.models : [];
				llmConfig.request_policies = Array.isArray(llmConfig.request_policies)
					? llmConfig.request_policies
					: [];
				hotkeyBinding = settings.hotkey?.key_binding || hotkeyBinding;
				hotkeyMode = settings.hotkey?.mode || 'toggle';
				muteHotkey = settings.hotkey?.mute_hotkey ?? null;
				session = {
					...session,
					...(settings.session || {}),
					max_steps_per_session: settings.session?.max_steps_per_session ?? null,
				};
				contextLimits = settings.context_limits || contextLimits;
				memory = { ...memory, ...(settings.memory || {}) };
				security = {
					permission_mode: settings.security?.permission_mode || 'default',
					sandbox_mode: settings.security?.sandbox_mode || 'workspace_write',
					network_policy: settings.security?.network_policy || 'ask',
					writable_roots: Array.isArray(settings.security?.writable_roots)
						? settings.security.writable_roots
						: [],
					permissions: Array.isArray(settings.security?.permissions)
						? settings.security.permissions
						: [],
				};
				const media = settings.media || {};
				mediaInputStrategy = media.input_strategy || 'auto';
				audio = media.audio || audio;
				stt = {
					provider: media.stt?.provider || 'llm',
					mcp_server: media.stt?.mcp_server || '',
					model: media.stt?.model || '',
					timeout_secs: media.stt?.timeout_secs || 30,
					min_confidence: media.stt?.min_confidence ?? 0.7,
				};
				ocr = {
					provider: media.ocr?.provider || 'llm',
					api_key: '',
					api_key_ref: media.ocr?.api_key_ref || null,
					api_secret: '',
					api_secret_ref: media.ocr?.api_secret_ref || null,
					base_url: media.ocr?.base_url || '',
					timeout_secs: media.ocr?.timeout_secs || 20,
					min_confidence: media.ocr?.min_confidence ?? 0.7,
				};
				tts = {
					provider: media.tts?.provider || 'none',
					model: media.tts?.model || '',
					voice: media.tts?.voice || '',
					timeout_secs: media.tts?.timeout_secs || 60,
				};
				imageGen = {
					provider: media.image_gen?.provider || 'none',
					model: media.image_gen?.model || '',
					timeout_secs: media.image_gen?.timeout_secs || 120,
				};
				mcpServerNames = (settings.mcp_servers || [])
					.map((server) => server.name || '')
					.filter(Boolean);
				notification = { ...notification, ...(settings.notification || {}) };
				setToolRunCompletionNotificationChannels(notification.tool_run_completed);
				log = { ...log, ...(settings.log || {}) };
				defaultShell = settings.default_shell || 'powershell';
				checkShells();
			}
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '加载设置失败', log: false });
		}
		try {
			sessionPermissions = await listSessionPermissions();
			if (!mounted) return;
		} catch (e) {
			reportError(e, {
				context: 'SettingsView',
				message: '加载会话授权失败',
				log: false,
			});
		}
		try {
			await refreshApiKeyStatus();
			if (!mounted) return;
		} catch (e) {
			reportError(e, {
				context: 'SettingsView',
				message: '获取 API Key 状态失败',
				log: false,
			});
		}
		if (mounted) settingsLoaded = true;
		try {
			autostartEnabled = await isAutostartEnabled();
			if (!mounted) return;
		} catch (e) {
			reportError(e, {
				context: 'SettingsView',
				message: '获取开机自启状态失败',
				log: false,
			});
		}
		if (mounted) {
			await tick();
			if (mounted) captureSnapshot();
		}
	});

	async function runMaintenance() {
		memoryMaintenance.running = true;
		try {
			memoryMaintenance.lastCount = await runMemoryMaintenance();
			addNotification(
				`记忆维护完成（清理 ${memoryMaintenance.lastCount} 项）`,
				'success',
				3000,
			);
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '记忆维护失败', log: false });
		} finally {
			memoryMaintenance.running = false;
		}
	}
	async function refreshApiKeyStatus() {
		const { providers, ...flags } = await readApiKeyStatus();
		keyConfigured = { ...keyConfigured, ...flags };
		keyConfiguredProviders = { ...providers };
	}
	async function revokePermission(key: string) {
		try {
			await revokePermissionCommand(key);
			security.permissions = security.permissions.filter(
				(permission) => permission.key !== key,
			);
			patchSnapshotSecurityPermissions(security.permissions);
			return true;
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '撤销权限失败', log: false });
			return false;
		}
	}
	async function revokeSessionPermission(grant: SessionPermissionGrant) {
		try {
			await revokeSessionPermissionCommand({
				sessionId: grant.session_id,
				capability: grant.capability,
			});
			sessionPermissions = sessionPermissions.filter(
				(current) =>
					current.session_id !== grant.session_id ||
					current.capability !== grant.capability,
			);
			return true;
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '撤销会话授权失败', log: false });
			return false;
		}
	}

	async function resetPermissions() {
		try {
			await resetPermissionsCommand();
			security.permissions = [];
			patchSnapshotSecurityPermissions([]);
			addNotification('权限规则已清除', 'success');
			return true;
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '清除权限规则失败', log: false });
			return false;
		}
	}
	async function resetSessionPermissions() {
		try {
			const removed = await resetSessionPermissionsCommand();
			sessionPermissions = [];
			addNotification(`已清除 ${removed} 条会话授权`, 'success');
			return true;
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '清除会话授权失败', log: false });
			return false;
		}
	}
	function setHotkeyMode(value: HotkeyModeInput) {
		hotkeyMode = value;
	}
	function setHotkeyBinding(value: string) {
		hotkeyBinding = value;
	}
	function setHotkeyCaptureActive(active: boolean) {
		void setHotkeyCaptureActiveCommand(active).catch((error) => {
			reportError(error, {
				context: 'SettingsView',
				message: active ? '暂停录音快捷键失败' : '恢复录音快捷键失败',
				log: false,
			});
		});
	}
	function setDefaultShell(value: ShellChoiceInput) {
		defaultShell = value;
	}
	function setAutostart(value: boolean) {
		autostartEnabled = value;
	}

	/** @returns {Promise<void>} */
	async function stageSettingsCredentials() {
		for (const provider of llmConfig.providers || []) {
			if (provider.api_key) {
				provider.api_key_ref = await stageProviderCredential({
					providerName: provider.name,
					apiKey: provider.api_key,
				});
			}
			delete provider.api_key;
		}
		if (ocr.api_key) {
			ocr.api_key_ref = await stageOcrCredential({
				apiSecret: false,
				value: ocr.api_key,
			});
			ocr.api_key = '';
		}
		if (ocr.api_secret) {
			ocr.api_secret_ref = await stageOcrCredential({
				apiSecret: true,
				value: ocr.api_secret,
			});
			ocr.api_secret = '';
		}
	}

	/** @returns {void} */
	function validateModelProviderBindings() {
		const providerNames = new Set((llmConfig.providers || []).map((provider) => provider.name));
		const invalidModels = (llmConfig.models || []).filter(
			(model) => !model.providerName || !providerNames.has(model.providerName),
		);
		if (invalidModels.length) {
			const modelIds = invalidModels.map((model) => model.id).join('、');
			throw new Error(`每个模型都必须绑定已配置的 Provider。请重新创建：${modelIds}`);
		}
	}

	/** @returns {Promise<boolean>} */
	async function saveSettings() {
		if (saveState === 'saving') return false;
		const securityChanged = dirtySettingsSectionIds.includes('security');
		saveState = 'saving';
		saveError = '';
		try {
			validateModelProviderBindings();
			await reconcileChatModelBeforeSave();
			await stageSettingsCredentials();
			skipNextChatModelSync = true;
			await updateSettingsCommand({
				settings:
					/** @type {import('$lib/contracts/settings.ts').SettingsUpdatePayload} */ {
						default_shell: defaultShell,
						llm: settingsLlmInputFromState(llmConfig),
						hotkey: {
							key_binding: hotkeyBinding,
							mode: hotkeyMode,
							mute_hotkey: muteHotkey,
						},
						session: {
							max_concurrent: session.max_concurrent,
							prompt_history_limit: session.prompt_history_limit,
							max_steps_per_run: session.max_steps_per_run,
							history_retention_days: session.history_retention_days,
							max_steps_per_session: session.max_steps_per_session ?? null,
						},
						memory: {
							fact_inference_enabled: memory.fact_inference_enabled,
						},
						security: {
							permission_mode: security.permission_mode,
							sandbox_mode: security.sandbox_mode,
							network_policy: security.network_policy,
							writable_roots: security.writable_roots,
							permissions: security.permissions,
						},
						context_limits: contextLimits,
						media: {
							input_strategy: mediaInputStrategy,
							audio: {
								sample_rate: audio.sample_rate,
								channels: audio.channels,
								bits_per_sample: audio.bits_per_sample,
								max_duration_secs: audio.max_duration_secs,
								silence_timeout_ms: audio.silence_timeout_ms,
								vad_threshold: audio.vad_threshold,
							},
							stt: {
								provider: stt.provider,
								mcp_server: stt.mcp_server || null,
								model: stt.model,
								timeout_secs: stt.timeout_secs,
								min_confidence: stt.min_confidence,
							},
							ocr: {
								provider: ocr.provider,
								api_key_ref: ocr.api_key_ref,
								api_secret_ref: ocr.api_secret_ref,
								base_url: ocr.base_url,
								timeout_secs: ocr.timeout_secs,
								min_confidence: ocr.min_confidence,
							},
							tts: {
								provider: tts.provider,
								model: tts.model,
								voice: tts.voice,
								timeout_secs: tts.timeout_secs,
							},
							image_gen: {
								provider: imageGen.provider,
								model: imageGen.model,
								timeout_secs: imageGen.timeout_secs,
							},
						},
						notification: {
							session_created: {
								in_app: notification.session_created.in_app,
								windows: notification.session_created.windows,
							},
							session_completed: {
								in_app: notification.session_completed.in_app,
								windows: notification.session_completed.windows,
							},
							session_paused: {
								in_app: notification.session_paused.in_app,
								windows: notification.session_paused.windows,
							},
							session_resumed: {
								in_app: notification.session_resumed.in_app,
								windows: notification.session_resumed.windows,
							},
							session_error: {
								in_app: notification.session_error.in_app,
								windows: notification.session_error.windows,
							},
							permission_requested: {
								in_app: notification.permission_requested.in_app,
								windows: notification.permission_requested.windows,
							},
							tool_run_completed: {
								in_app: notification.tool_run_completed.in_app,
								windows: notification.tool_run_completed.windows,
							},
						},
						log: {
							level: log.level,
							file_enabled: log.file_enabled,
							file_path: log.file_path ?? null,
						},
					},
			});
			if (securityChanged) {
				securityRuntimeStatus = 'current';
				securityRuntimeNotice = '安全策略已按当前配置完成运行时应用。';
			}
			setToolRunCompletionNotificationChannels(notification.tool_run_completed);
			addNotification('设置已写入配置', 'success');
			try {
				await refreshApiKeyStatus();
			} catch (e) {
				reportError(e, {
					context: 'SettingsView',
					message: '获取 API Key 状态失败',
					log: false,
				});
			}
			try {
				if (autostartEnabled) await enableAutostart();
				else await disableAutostart();
			} catch (e) {
				autostartEnabled = !autostartEnabled;
				reportError(e, {
					context: 'SettingsView',
					message: '自动启动设置失败',
					log: false,
				});
			}
			if (mounted) captureSnapshot();
			saveState = 'saved';
			return true;
		} catch (e) {
			skipNextChatModelSync = false;
			saveState = 'error';
			const partialApplyFailure = isPartialConfigApplyError(e);
			saveError = partialApplyFailure ? PARTIAL_APPLY_SAVE_MESSAGE : formatError(e);
			if (partialApplyFailure && securityChanged) {
				const detail = formatError(e);
				const configVersion = detail.match(/config_version=(\d+)/)?.[1];
				const activeVersion = detail.match(/security_base_version=(\d+)/)?.[1];
				if (detail.includes('security_runtime=unchanged')) {
					securityRuntimeStatus = 'unchanged';
					securityRuntimeNotice = `磁盘配置版本 ${configVersion || '未知'} 已写入，但当前进程仍以最后完整应用的安全配置${activeVersion ? `（版本 ${activeVersion}）` : ''}为基础；重启前实际采用该旧策略，之后单独确认的权限规则仍即时生效。`;
				} else if (detail.includes('security_runtime=incomplete_fail_closed')) {
					securityRuntimeStatus = 'incomplete';
					securityRuntimeNotice = `磁盘配置版本 ${configVersion || '未知'} 已写入；当前进程已更新安全边界，但会话授权恢复失败并保持 fail-closed。重启后会按已保存配置重新初始化并恢复持久授权。`;
				} else {
					securityRuntimeStatus = 'current';
					securityRuntimeNotice = `安全策略已在配置版本 ${configVersion || '未知'} 完成运行时应用；其他运行时更新未完成。`;
				}
			}
			if (partialApplyFailure && mounted) captureSnapshot();
			reportError(e, {
				context: 'SettingsView',
				message: partialApplyFailure ? PARTIAL_APPLY_SAVE_MESSAGE : '保存设置失败',
				log: false,
			});
			return false;
		}
	}

	async function handleSaveClick() {
		const action = resolveSettingsSaveAction(settingsLoaded, settingsDirty);
		if (action === 'loading') {
			addNotification('设置仍在加载，请稍候', 'info', 2500);
			return;
		}
		if (action === 'empty') {
			addNotification('没有需要保存的设置', 'info', 2500);
			return;
		}
		await saveSettings();
	}

	function handleSaveBarHeightChange(height: number) {
		saveBarHeight = height;
	}

	$effect(() => {
		const content = settingsViewElement?.closest<HTMLElement>('.content');
		if (!content) return;
		if (saveBarVisible) {
			content.style.setProperty('--settings-save-bar-clearance', `${saveBarHeight}px`);
		} else {
			content.style.removeProperty('--settings-save-bar-clearance');
		}
		return () => content.style.removeProperty('--settings-save-bar-clearance');
	});
</script>

<div
	class="settings-view-shell"
	class:settings-view-shell--save-bar-visible={saveBarVisible}
	bind:this={settingsViewElement}
>
	<div class="settings-surface-slot">
		<WorkspaceSurface {entering} {onAnimationEnd}>
			<div class="settings-page">
				<WorkspacePageHeader
					title="设置"
					description="按用途分组管理 Haven 配置；修改分类后可以继续浏览，离开页面时会提醒保存。"
				/>
				<div
					class="settings-layout workspace-secondary-layout responsive-layout-transition"
				>
					<aside
						class="settings-sidebar workspace-secondary-sidebar responsive-layout-panel"
					>
						<MaterialTabs
							tabs={settingsTabs}
							activeTab={settingsTab}
							onNavigate={changeSettingsTab}
							ariaLabel="设置分类"
							idPrefix="settings-tab"
							panelId="settings-panel"
							className="workspace-secondary-tabs workspace-secondary-tabs--sidebar"
							{isVisible}
						/>
					</aside>
					<div class="settings-main workspace-secondary-main">
						<div
							id="settings-panel"
							role="tabpanel"
							aria-label={activeSettingsSection.label}
						>
							<div class="settings-panel-heading">
								<div>
									<h2>{activeSettingsSection.label}</h2>
									<p>{activeSettingsSection.description}</p>
								</div>
								{#if dirtySettingsSectionIds.includes(settingsTab)}
									<span class="settings-dirty-badge">有未保存修改</span>
								{/if}
							</div>
							{#if visitedSettingsTabs.includes('behavior')}
								<div hidden={settingsTab !== 'behavior'}>
									<SettingsBehavior
										{hotkeyMode}
										{hotkeyBinding}
										{session}
										{defaultShell}
										{shellAvailable}
										{memory}
										{memoryMaintenance}
										onHotkeyModeChange={setHotkeyMode}
										onHotkeyBindingChange={setHotkeyBinding}
										onHotkeyCaptureChange={setHotkeyCaptureActive}
										onDefaultShellChange={setDefaultShell}
										onRunMaintenance={runMaintenance}
									/>
								</div>
							{/if}
							{#if visitedSettingsTabs.includes('appearance')}
								<div hidden={settingsTab !== 'appearance'}>
									<SettingsAppearance
										{notification}
										{autostartEnabled}
										onAutostartChange={setAutostart}
									/>
								</div>
							{/if}
							{#if visitedSettingsTabs.includes('diagnostics')}
								<div hidden={settingsTab !== 'diagnostics'}>
									<SettingsDiagnostics {log} />
								</div>
							{/if}
							{#if visitedSettingsTabs.includes('models') || visitedSettingsTabs.includes('media')}
								<div hidden={settingsTab !== 'models' && settingsTab !== 'media'}>
									{#if settingsLoaded}
										<ModelSettings
											section={modelSection}
											active={settingsTab === 'models' ||
												settingsTab === 'media'}
											{llmConfig}
											{audio}
											{stt}
											{ocr}
											{tts}
											{imageGen}
											{mediaInputStrategy}
											{contextLimits}
											{keyConfigured}
											{keyConfiguredProviders}
											{mcpServerNames}
											loaded={true}
											onDiscoverySettled={reBaselineAfterDiscovery}
											onProviderDiscoveryFailure={(
												/** @type {string} */ providerName,
												/** @type {boolean} */ staticCatalog,
											) => {
												providerDiscoveryAlert = {
													providerName,
													staticCatalog,
												};
											}}
										/>
									{:else}
										<p class="model-hint">正在加载模型与 API Key 状态…</p>
									{/if}
								</div>
							{/if}
							{#if visitedSettingsTabs.includes('security')}
								<div hidden={settingsTab !== 'security'}>
									<SettingsSecurity
										{security}
										{sessionPermissions}
										securityDirty={dirtySettingsSectionIds.includes('security')}
										{securityRuntimeStatus}
										{securityRuntimeNotice}
										onRevokePermission={revokePermission}
										onRevokeSessionPermission={revokeSessionPermission}
										onResetPermissions={resetPermissions}
										onResetSessionPermissions={resetSessionPermissions}
									/>
								</div>
							{/if}
							{#if visitedSettingsTabs.includes('limits')}
								<div hidden={settingsTab !== 'limits'}>
									<SettingsLimits {contextLimits} />
								</div>
							{/if}
						</div>
					</div>
				</div>
			</div>
		</WorkspaceSurface>
	</div>
	<div
		class="settings-save-bar-slot"
	>
		<SettingsSaveBar
			visible={saveBarVisible}
			dirty={settingsDirty}
			dirtySectionLabels={dirtySettingsSectionLabels}
			{saveState}
			{saveError}
			onHeightChange={handleSaveBarHeightChange}
			onDiscard={discardAndReset}
			onSave={handleSaveClick}
		/>
	</div>
</div>

<MaterialDialog open={leaveDialogOpen} title="未保存的更改" onClose={stayOnSettings}>
	{#snippet children()}<p>
			设置已修改但尚未保存。你可以放弃更改并离开，或先保存再离开。
		</p>{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="text"
			label="放弃并离开"
			onclick={leaveWithoutSaving}
			disabled={leaveSaving}
		/>
		<MaterialButton
			variant="filled"
			label={leaveSaving ? '保存中…' : '保存并离开'}
			onclick={leaveWithSaving}
			disabled={leaveSaving}
		/>
	{/snippet}
</MaterialDialog>

<MaterialDialog
	open={!!providerDiscoveryAlert.providerName}
	title={providerDiscoveryAlert.staticCatalog ? '无法在线验证 API Key' : '模型列表获取失败'}
	onClose={() => (providerDiscoveryAlert = { providerName: '', staticCatalog: false })}
>
	{#snippet children()}
		<div class="provider-discovery-notice" role="alert">
			{#if providerDiscoveryAlert.staticCatalog}
				<p>
					Provider「{providerDiscoveryAlert.providerName}」已添加。该协议没有在线模型列表接口，当前显示内置模型目录，因此无法验证
					API Key。
				</p>
				<p>请确认 API Key 可用；也可在「模型」设置中添加模型并手动输入模型 ID。</p>
			{:else}
				<p>
					Provider「{providerDiscoveryAlert.providerName}」已添加，但无法验证 API Key
					或获取模型列表。请检查 API Key、Base URL 和 Provider 预设。
				</p>
				<p>
					部分服务不开放 <code>/models</code> 接口；你仍可在「模型」设置中添加模型并手动输入模型
					ID。
				</p>
			{/if}
		</div>
	{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="filled"
			label="知道了"
			onclick={() => (providerDiscoveryAlert = { providerName: '', staticCatalog: false })}
		/>
	{/snippet}
</MaterialDialog>

<style>
	.settings-view-shell {
		--settings-surface-bottom-gap: var(
			--workspace-surface-bottom-gap,
			var(--md-sys-content-gutter)
		);
		display: grid;
		flex: 1 1 auto;
		grid-template-columns: minmax(0, 1fr);
		grid-template-rows: minmax(0, 1fr);
		width: 100%;
		min-width: 0;
		min-height: 100%;
	}
	.settings-surface-slot {
		grid-column: 1;
		grid-row: 1;
		display: flex;
		min-width: 0;
		min-height: 0;
		margin-bottom: var(--settings-surface-bottom-gap);
	}
	.settings-surface-slot :global(.workspace-surface) {
		flex: 1 1 auto;
		min-height: 0;
	}
	.settings-save-bar-slot {
		position: fixed;
		inset-inline: 0;
		bottom: 0;
		z-index: var(--md-sys-z-drawer);
		width: 100%;
		min-width: 0;
		display: flex;
		justify-content: center;
		pointer-events: none;
	}
	:global(.content:not(.content--chat) .page-shell:has(.settings-view-shell)) {
		display: flex;
		flex-direction: column;
		min-height: 100%;
	}
	:global(.content:not(.content--chat):has(.tab-panel:not([hidden]) .settings-view-shell)) {
		padding-bottom: 0;
	}
	:global(
		.content:not(.content--chat):has(
				.tab-panel:not([hidden]) .settings-view-shell--save-bar-visible
			)
	) {
		padding-bottom: var(--settings-save-bar-clearance, 0px);
		transition: padding-bottom 0s;
	}
	.settings-page {
		display: flex;
		flex: 1;
		flex-direction: column;
		width: 100%;
		min-width: 0;
		max-width: var(--md-sys-content-max-width);
		padding-bottom: 0;
	}
	:global(.settings-page .md-btn[data-width='content']:not(.accent-swatch)) {
		min-width: min(100%, var(--md-comp-control-compact-width));
	}
	.settings-layout {
		flex: 1;
		min-width: 0;
		grid-template-rows: auto minmax(0, 1fr);
		align-items: stretch;
	}
	.settings-main {
		display: flex;
		flex-direction: column;
		min-width: 0;
		width: 100%;
		max-width: var(--md-sys-content-max-width);
		margin-inline: auto;
		container-name: settings-content;
		container-type: inline-size;
	}
	.settings-panel-heading {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		margin: 0 0 var(--md-sys-space-xl);
		padding-bottom: var(--md-sys-space-md);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.settings-panel-heading h2 {
		margin: 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-headline-medium-size);
		line-height: var(--md-sys-typescale-headline-medium-line-height);
	}
	.settings-panel-heading p {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.settings-dirty-badge {
		flex: 0 0 auto;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-tertiary-container);
		color: var(--md-sys-color-on-tertiary-container);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.provider-discovery-notice {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-error);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.provider-discovery-notice p {
		margin: 0;
	}
	.provider-discovery-notice code {
		padding: 1px 4px;
		border-radius: var(--md-sys-shape-extra-small);
		background: color-mix(in srgb, var(--md-sys-color-on-error-container) 8%, transparent);
	}
	@media screen and (min-width: 840px) {
		.settings-page {
			max-width: none;
		}
		.settings-layout {
			grid-template-rows: minmax(0, 1fr);
		}
	}
	@container settings-content (max-width: 640px) {
		.settings-panel-heading {
			flex-direction: column;
		}
	}
</style>
