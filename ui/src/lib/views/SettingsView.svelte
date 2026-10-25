<script lang="ts">
	/**
	 * Owns the SettingsSnapshot draft, save baseline, and leave guard. Saves send
	 * only changed top-level sections through SettingsPatch.
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
	import { page } from '$app/stores';
	import { appEventListeners, registerListeners } from '$lib/events.ts';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialTabs from '$lib/MaterialTabs.svelte';
	import LoadingState from '$lib/LoadingState.svelte';
	import SettingsErrorNotice from '$lib/SettingsErrorNotice.svelte';
	import { addNotification } from '$lib/notificationStore.ts';
	import { executionWorkspaceStore } from '$lib/executionWorkspaceStore.ts';
	import { publishNotificationSettingsChanged } from '$lib/notificationSettingsEvents.ts';
	import { validateSettingsModelDrafts } from '$lib/settingsModelValidation.ts';
	import { formatError } from '$lib/formatError.ts';
	import { isPartialConfigApplyError, presentSettingsFailure } from '$lib/settingsSaveFailure.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { registerSettingsLeaveGuard } from '$lib/settingsGuard.ts';
	import { selectSettingsSaveAction } from '$lib/settingsSaveAction.ts';
	import {
		disableAutostart,
		discardStagedCredentials,
		enableAutostart,
		isAutostartEnabled,
		getSettingsRuntimeApplyStatus,
		listSessionPermissions,
		loadSettings,
		pickExecutionWorkspaceDirectory as pickExecutionWorkspaceDirectoryCommand,
		resetPermissions as resetPermissionsCommand,
		resetSessionPermissions as resetSessionPermissionsCommand,
		revokePermission as revokePermissionCommand,
		revokeSessionPermission as revokeSessionPermissionCommand,
		reapplySavedSettings,
		runMemoryMaintenance,
		setHotkeyCaptureActive as setHotkeyCaptureActiveCommand,
		stageIndependentSearchCredential,
		stageOcrCredential,
		stageProviderCredential,
		updateSettings as updateSettingsCommand,
	} from '$lib/settingsCommands.ts';
	import { checkShellAvailable, readApiKeyStatus } from '$lib/diagnosticsCommands.ts';
	import type { ApiKeyStatus } from '$lib/contracts/diagnostics.ts';
	import {
		SHELL_CHOICE_INPUT_VALUES,
		type TauriCommandResponse,
	} from '$lib/contracts/generatedCommands.ts';
	import ModelSettings from './ModelSettings.svelte';
	import type { DiscoveredModelMetadataFill } from '$lib/modelDiscovery.ts';
	import SettingsBehavior from './SettingsBehavior.svelte';
	import SettingsAppearance from './SettingsAppearance.svelte';
	import SettingsDiagnostics from './SettingsDiagnostics.svelte';
	import SettingsAbout from './SettingsAbout.svelte';
	import SettingsSecurity from './SettingsSecurity.svelte';
	import SettingsHooks from './SettingsHooks.svelte';
	import SettingsLimits from './SettingsLimits.svelte';
	import SettingsSaveBar from './SettingsSaveBar.svelte';
	import WorkspacePageHeader from '$lib/WorkspacePageHeader.svelte';
	import WorkspaceSectionHeader from '$lib/WorkspaceSectionHeader.svelte';
	import WorkspaceSearch from '$lib/WorkspaceSearch.svelte';
	import WorkspaceSurface from '$lib/WorkspaceSurface.svelte';
	import type {
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
		ExecutionModeInput,
		SecurityConfigInput,
		SessionConfigInput,
		ShellChoiceInput,
		SttConfigInput,
		TtsConfigInput,
	} from '$lib/contracts/generatedCommands.ts';
	import type { SessionTokenStatsView } from '$lib/sessionUsagePresentation.ts';
	import type { SessionLlmUsage } from '$lib/contracts/sessionHistory.ts';
	import type {
		ImageGenConfigDraft,
		SttConfigDraft,
		TtsConfigDraft,
	} from '$lib/mediaConfigDraft.ts';
	import type { StoredPermission } from '$lib/contracts/generatedCommands.ts';
	import type { SecuritySettingsDraft, SessionPermissionGrant } from '$lib/contracts/settings.ts';
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
	let contextLimits = $state<Required<ContextLimitsConfigInput>>({
		compaction_ratio: 0.65,
		compaction_reserve_tokens: 8192,
		default_context_window: 64000,
		response_token_floor: 32000,
		max_observation_chars: 16000,
		max_transcript_chars: 4000,
		max_attachment_images: 4,
		max_attachment_files: 5,
		max_attachment_image_bytes: 10 * 1024 * 1024,
		max_attachment_file_bytes: 20 * 1024 * 1024,
		max_upload_total_bytes: 512 * 1024 * 1024,
		max_attachment_image_dim_px: 1568,
		attachment_image_jpeg_quality: 0.85,
		file_max_full_file_bytes: 128000,
		file_max_patch_bytes: 8 * 1024 * 1024,
		file_line_span: 100,
		file_line_max_bytes: 128000,
		file_summary_input_chars: 60000,
		file_max_list_entries: 1000,
		file_byte_read_max_bytes: 16 * 1024 * 1024,
		file_write_max_bytes: 16 * 1024 * 1024,
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
		foreground_tool_output_tail_max_chars: 2000,
		foreground_tool_output_emit_interval_ms: 1500,
		tool_run_terminal_ttl_secs: 600,
		mcp_max_binary_base64_bytes: 2 * 1024 * 1024,
		mcp_max_sse_buffer_bytes: 2 * 1024 * 1024,
		skills_max_md_bytes: 256 * 1024,
		skills_max_parse_lines: 5000,
		skills_max_line_bytes: 4096,
		self_tool_max_instructions_bytes: 256 * 1024,
		self_tool_max_script_bytes: 512 * 1024,
		network_max_retries: 2,
		network_backoff_base_secs: 1,
		network_max_body_bytes: 1024 * 1024,
		clipboard_history_entries: 10,
		clipboard_history_max_entries: 100,
		clipboard_entry_max_chars: 2000,
		max_running_background_tool_runs: 64,
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
	let security = $state<SecuritySettingsDraft>({
		execution_mode: 'collaborative',
		resource_policy: {
			files: {
				write_mode: 'workspace',
				write_roots: [],
				path_roots: [],
				read_only_roots: [],
				denied_paths: [],
			},
			network: { access: 'restricted', allowed_domains: [] },
		},
		execution_root: null,
		permissions: [],
	});
	let sessionPermissions = $state<SessionPermissionGrant[]>([]);
	let stt = $state<SttConfigDraft>({
		providerName: 'llm',
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
	let tts = $state<TtsConfigDraft>({
		providerName: 'none',
		model: '',
		voice: '',
		timeout_secs: 60,
	});
	let imageGen = $state<ImageGenConfigDraft>({
		providerName: 'none',
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
			id: 'hooks',
			label: '生命周期 Hooks',
			description: '配置 Agent 生命周期命令、项目目录信任和 Hook 运行诊断。',
			keys: [],
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
		{
			id: 'about',
			label: '关于',
			description: '查看项目链接、许可证、第三方组件和致谢。',
			keys: [],
		},
	] as const;
	type SettingsTabId = (typeof SETTINGS_SECTIONS)[number]['id'];
	type SettingsSearchResult = {
		id: string;
		label: string;
		section: SettingsTabId;
		sectionLabel: string;
		description: string;
		targetId?: string;
	};
	type HookDraftState = {
		dirty: boolean;
		save: () => Promise<boolean>;
		discard: () => void;
	};
	let hookDraftState = $state<HookDraftState>({
		dirty: false,
		save: async () => true,
		discard: () => {},
	});
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
			const dirtySections = SETTINGS_SECTIONS.filter((section) =>
				section.keys.some(
					(key) => JSON.stringify(current[key]) !== JSON.stringify(baseline[key]),
				),
			).map((section) => section.id);
			if (hookDraftState.dirty && !dirtySections.includes('hooks'))
				dirtySections.push('hooks');
			return dirtySections;
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
	let settingsSearchQuery = $state('');
	let settingsSearchResults = $state<SettingsSearchResult[]>([]);
	let settingsSearchLoading = $state(false);
	let settingsSearchSequence = 0;
	let highlightedSearchTarget: HTMLElement | null = null;
	let settingsSearchHighlightTimer: ReturnType<typeof setTimeout> | null = null;
	let savedSnapshot = $state('');
	let leaveDialogOpen = $state(false);
	let leaveDialogResolve: ((ok: boolean) => void) | null = null;
	let leaveSaving = $state(false);
	let saveState = $state<'idle' | 'saving' | 'saved' | 'error'>('idle');
	let providerSaveBusy = $state(false);
	let settingsSaveSettled: Promise<void> | null = null;
	let saveError = $state<ReturnType<typeof presentSettingsFailure> | null>(null);
	let settingsApplyStatuses = $state<TauriCommandResponse<'get_settings_runtime_apply_status'>>(
		[],
	);
	let reapplyBusy = $state(false);
	let reapplyError = $state<ReturnType<typeof presentSettingsFailure> | null>(null);
	let visibleApplyStatuses = $derived(
		settingsApplyStatuses.filter((status) => status.result !== 'applied'),
	);
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
		if (field === 'web_search') return model.web_search ?? null;
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
				execution_mode: security.execution_mode,
				resource_policy: security.resource_policy,
				execution_root: security.execution_root,
				permissions: security.permissions,
			},
			context_limits: contextLimits,
			media: {
				input_strategy: mediaInputStrategy,
				audio: {
					max_duration_secs: asNumber(audio.max_duration_secs),
					silence_timeout_ms: asNumber(audio.silence_timeout_ms),
					vad_threshold: asNumber(audio.vad_threshold),
				},
				stt: {
					provider_name: stt.providerName,
					mcp_server: stt.mcp_server || null,
					model: stt.model,
					timeout_secs: asNumber(stt.timeout_secs),
					min_confidence: asNumber(stt.min_confidence),
				},
				ocr: {
					provider: ocr.provider,
					api_key_ref: ocr.api_key_ref,
					api_secret_ref: ocr.api_secret_ref,
					base_url: ocr.base_url,
					timeout_secs: asNumber(ocr.timeout_secs),
					min_confidence: asNumber(ocr.min_confidence),
				},
				tts: {
					provider_name: tts.providerName,
					model: tts.model,
					voice: tts.voice,
					timeout_secs: asNumber(tts.timeout_secs),
				},
				image_gen: {
					provider_name: imageGen.providerName,
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
	function isSettingsRecord(value: unknown): value is Record<string, unknown> {
		return typeof value === 'object' && value !== null && !Array.isArray(value);
	}
	function mergeChangedSettingsFields(
		baseline: unknown,
		draft: unknown,
		latest: unknown,
	): unknown {
		if (JSON.stringify(draft) === JSON.stringify(baseline)) return latest;
		if (!isSettingsRecord(baseline) || !isSettingsRecord(draft) || !isSettingsRecord(latest)) {
			return draft;
		}

		const merged = { ...latest };
		for (const [key, draftValue] of Object.entries(draft)) {
			if (JSON.stringify(draftValue) !== JSON.stringify(baseline[key])) {
				merged[key] = mergeChangedSettingsFields(baseline[key], draftValue, latest[key]);
			}
		}
		return merged;
	}
	function isSettingsFormDirty() {
		return (
			settingsLoaded &&
			!!savedSnapshot &&
			JSON.stringify(buildPersistableSettings()) !== savedSnapshot
		);
	}
	function isDirty() {
		return isSettingsFormDirty() || hookDraftState.dirty;
	}
	const settingsDirty = $derived.by(() => isSettingsFormDirty());
	const anySettingsDirty = $derived.by(() => isDirty());

	function discardAndReset() {
		discardChanges();
		hookDraftState.discard();
		saveState = 'idle';
		saveError = null;
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

	async function searchSettings(value: string) {
		settingsSearchQuery = value;
		const query = value.trim().toLocaleLowerCase();
		const sequence = ++settingsSearchSequence;
		settingsSearchResults = [];
		settingsSearchLoading = Boolean(query);
		if (!query) return;

		// Search every category after the first query so a result can lead directly
		// to a field in a category the user has never opened.
		visitedSettingsTabs = SETTINGS_SECTIONS.map((section) => section.id);
		await tick();
		await tick();
		if (sequence !== settingsSearchSequence) return;

		const fieldResults: SettingsSearchResult[] = [];
		const categoryResults: SettingsSearchResult[] = [];
		let nextIndex = 0;
		for (const section of SETTINGS_SECTIONS) {
			const categoryText = `${section.label} ${section.description}`.toLocaleLowerCase();
			if (categoryText.includes(query)) {
				categoryResults.push({
					id: `settings-search-${sequence}-section-${section.id}`,
					label: section.label,
					section: section.id,
					sectionLabel: section.label,
					description: section.description,
				});
			}
		}

		const fields = document.querySelectorAll<HTMLElement>(
			'#settings-panel .settings-field, #settings-panel .limit-row, #settings-panel [data-setting-searchable]',
		);
		for (const field of fields) {
			const sectionId =
				field.closest<HTMLElement>('[data-settings-section]')?.dataset.settingsSection;
			if (!sectionId || !isSettingsTabId(sectionId)) continue;
			const section = SETTINGS_SECTIONS.find((item) => item.id === sectionId);
			if (!section) continue;
			const label =
				field
					.querySelector<HTMLElement>(
						'.settings-field__label label, .settings-field__label span, .limit-label label, .md-collapsible-header',
					)
					?.textContent?.trim() || section.label;
			const description =
				field
					.querySelector<HTMLElement>('.settings-field__label small, .limit-hint')
					?.textContent?.trim() || '';
			const searchable =
				`${label} ${description} ${section.label} ${field.textContent || ''}`.toLocaleLowerCase();
			if (!searchable.includes(query)) continue;
			const targetId = `settings-search-${sequence}-hit-${nextIndex++}`;
			field.dataset.settingsSearchId = targetId;
			fieldResults.push({
				id: targetId,
				label,
				section: section.id,
				sectionLabel: section.label,
				description,
				targetId,
			});
		}
		settingsSearchResults = [...fieldResults, ...categoryResults].slice(0, 12);
		settingsSearchLoading = false;
	}

	async function openSettingsSearchResult(result: SettingsSearchResult) {
		changeSettingsTab(result.section);
		await tick();
		if (!result.targetId) {
			document.querySelector('#settings-panel')?.scrollIntoView?.({
				behavior: 'smooth',
				block: 'start',
			});
			return;
		}
		const field = document.querySelector<HTMLElement>(
			`#settings-panel [data-settings-search-id="${result.targetId}"]`,
		);
		if (!field) return;
		let collapsible = field.closest<HTMLElement>('.md-collapsible');
		while (collapsible) {
			const header = collapsible.querySelector<HTMLButtonElement>(
				':scope > .md-collapsible-header',
			);
			if (header?.getAttribute('aria-expanded') === 'false') header.click();
			collapsible =
				collapsible.parentElement?.closest<HTMLElement>('.md-collapsible') ?? null;
		}
		await tick();
		field.scrollIntoView?.({ behavior: 'smooth', block: 'center' });
		field.classList.add('settings-search-target');
		field
			.querySelector<HTMLElement>('input:not([type="hidden"]), select, textarea, button')
			?.focus();
		if (highlightedSearchTarget && highlightedSearchTarget !== field) {
			highlightedSearchTarget.classList.remove('settings-search-target');
		}
		highlightedSearchTarget = field;
		if (settingsSearchHighlightTimer) clearTimeout(settingsSearchHighlightTimer);
		settingsSearchHighlightTimer = setTimeout(() => {
			field.classList.remove('settings-search-target');
			highlightedSearchTarget = null;
		}, 2200);
	}

	$effect(() => {
		const url = $page?.url;
		if (url?.searchParams.get('tab') !== 'settings') return;
		const section = url.searchParams.get('section');
		if (section && isSettingsTabId(section) && section !== settingsTab) {
			changeSettingsTab(section);
		}
	});

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
		if (saveState === 'saving' || providerSaveBusy) return;
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
					resource_policy: {
						files: {
							...security.resource_policy.files,
							...(snapshot.security.resource_policy?.files || {}),
						},
						network: {
							...security.resource_policy.network,
							...(snapshot.security.resource_policy?.network || {}),
						},
					},
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
					providerName: snapshot.media.stt.provider_name || 'llm',
					mcp_server: snapshot.media.stt.mcp_server || '',
					model: snapshot.media.stt.model || '',
					timeout_secs: snapshot.media.stt.timeout_secs || 30,
					min_confidence: snapshot.media.stt.min_confidence ?? 0.7,
				};
			if (snapshot.media?.ocr) ocr = { ...ocr, ...snapshot.media.ocr };
			if (snapshot.media?.tts)
				tts = {
					providerName: snapshot.media.tts.provider_name || 'none',
					model: snapshot.media.tts.model || '',
					voice: snapshot.media.tts.voice || '',
					timeout_secs: snapshot.media.tts.timeout_secs || 60,
				};
			if (snapshot.media?.image_gen)
				imageGen = {
					providerName: snapshot.media.image_gen.provider_name || 'none',
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
		if (saveState === 'saving' || providerSaveBusy) return;
		discardChanges();
		hookDraftState.discard();
		finishLeaveDialog(true);
	}
	async function leaveWithSaving() {
		if (saveState === 'saving' || providerSaveBusy) return;
		leaveSaving = true;
		if (await saveAllChanges()) finishLeaveDialog(true);
		else leaveSaving = false;
	}
	function stayOnSettings() {
		if (!leaveSaving) finishLeaveDialog(false);
	}

	onDestroy(() => {
		mounted = false;
		if (settingsSearchHighlightTimer) clearTimeout(settingsSearchHighlightTimer);
		highlightedSearchTarget?.classList.remove('settings-search-target');
		if (settingsSaveSettled) {
			void settingsSaveSettled.then(requestDiscardStagedCredentials);
		} else {
			requestDiscardStagedCredentials();
		}
		eventRegistrations?.dispose();
		eventRegistrations = null;
		registerSettingsLeaveGuard(null);
		if (leaveDialogResolve) {
			leaveDialogResolve(false);
			leaveDialogResolve = null;
		}
	});

	onMount(async () => {
		executionWorkspaceStore.refresh();
		registerSettingsLeaveGuard({ isDirty, confirmLeave });
		eventRegistrations = registerListeners(
			appEventListeners({
				'llm:config_changed': () => {
					if (skipNextChatModelSync) {
						skipNextChatModelSync = false;
						return;
					}
					syncChatModelFromBackend();
				},
			}),
			{ tag: 'SettingsView' },
		);
		let settingsLoadedSuccessfully = false;
		try {
			const settings = await loadSettings();
			if (!mounted) return;
			if (!settings) throw new Error('Invalid get_settings response');
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
				execution_mode: settings.security?.execution_mode || 'collaborative',
				resource_policy: settings.security?.resource_policy || {
					files: {
						write_mode: 'workspace',
						write_roots: [],
						path_roots: [],
						read_only_roots: [],
						denied_paths: [],
					},
					network: { access: 'restricted', allowed_domains: [] },
				},
				execution_root: settings.security?.execution_root ?? null,
				permissions: Array.isArray(settings.security?.permissions)
					? settings.security.permissions
					: [],
			};
			const media = settings.media || {};
			mediaInputStrategy = media.input_strategy || 'auto';
			audio = media.audio || audio;
			stt = {
				providerName: media.stt?.provider_name || 'llm',
				mcp_server: media.stt?.mcp_server || '',
				model: media.stt?.model || '',
				timeout_secs: media.stt?.timeout_secs ?? 30,
				min_confidence: media.stt?.min_confidence ?? 0.7,
			};
			ocr = {
				provider: media.ocr?.provider || 'llm',
				api_key: '',
				api_key_ref: media.ocr?.api_key_ref || null,
				api_secret: '',
				api_secret_ref: media.ocr?.api_secret_ref || null,
				base_url: media.ocr?.base_url || '',
				timeout_secs: media.ocr?.timeout_secs ?? 20,
				min_confidence: media.ocr?.min_confidence ?? 0.7,
			};
			tts = {
				providerName: media.tts?.provider_name || 'none',
				model: media.tts?.model || '',
				voice: media.tts?.voice || '',
				timeout_secs: media.tts?.timeout_secs ?? 60,
			};
			imageGen = {
				providerName: media.image_gen?.provider_name || 'none',
				model: media.image_gen?.model || '',
				timeout_secs: media.image_gen?.timeout_secs ?? 120,
			};
			mcpServerNames = (settings.mcp_servers || [])
				.map((server) => server.name || '')
				.filter(Boolean);
			notification = { ...notification, ...(settings.notification || {}) };
			log = { ...log, ...(settings.log || {}) };
			defaultShell = settings.default_shell || 'powershell';
			checkShells();
			settingsLoadedSuccessfully = true;
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '加载设置失败', log: false });
		}
		await refreshSettingsApplyStatus();
		if (!mounted) return;
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
		if (mounted && settingsLoadedSuccessfully) {
			await tick();
			if (mounted) {
				captureSnapshot();
				settingsLoaded = true;
			}
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
	async function pickExecutionWorkspace(): Promise<string | null> {
		try {
			const selected = await pickExecutionWorkspaceDirectoryCommand();
			if (selected !== null) executionWorkspaceStore.add(selected);
			return selected;
		} catch (error) {
			reportError(error, {
				context: 'SettingsView',
				message: '选择执行工作区失败',
				log: false,
			});
			return null;
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
				sessionId: grant.sessionId,
				capability: grant.capability,
			});
			sessionPermissions = sessionPermissions.filter(
				(current) =>
					current.sessionId !== grant.sessionId ||
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
		for (const model of llmConfig.models || []) {
			if (model.independentSearchApiKey) {
				const search = model.independent_search;
				if (search?.provider !== 'aliyun_ai_search') {
					throw new Error(
						'Alibaba AI Search key requires Alibaba AI Search to be enabled',
					);
				}
				search.api_key_ref = await stageIndependentSearchCredential({
					modelId: model.id,
					apiKey: model.independentSearchApiKey,
				});
				model.independentSearchApiKey = '';
			}
		}
		if (ocr.api_key) {
			ocr.api_key_ref = await stageOcrCredential({
				field: 'api_key',
				value: ocr.api_key,
			});
			ocr.api_key = '';
		}
		if (ocr.api_secret) {
			ocr.api_secret_ref = await stageOcrCredential({
				field: 'api_secret',
				value: ocr.api_secret,
			});
			ocr.api_secret = '';
		}
	}

	/** @returns {void} */
	function validateModelProviderBindings() {
		validateSettingsModelDrafts(llmConfig.models, llmConfig.providers);
	}

	async function refreshSettingsApplyStatus() {
		try {
			settingsApplyStatuses = await getSettingsRuntimeApplyStatus();
		} catch (error) {
			reportError(error, {
				context: 'SettingsView',
				message: '读取运行时配置状态失败',
				log: false,
			});
		}
	}

	async function reapplySavedRuntimeTargets() {
		if (reapplyBusy || saveState === 'saving') return;
		reapplyBusy = true;
		reapplyError = null;
		try {
			settingsApplyStatuses = await reapplySavedSettings();
			addNotification('已保存配置已重新应用', 'success');
		} catch (error) {
			const failure = presentSettingsFailure(error, 'reapply');
			reapplyError = failure;
			await refreshSettingsApplyStatus();
			reportError(error, {
				context: 'SettingsView',
				message: failure.title,
				log: false,
				notificationMessage: `${failure.title}：${failure.message}`,
				notificationType: failure.tone,
			});
		} finally {
			reapplyBusy = false;
		}
	}

	function runtimeTargetLabel(target: string): string {
		const labels: Record<string, string> = {
			input_pipeline: '录音与媒体输入',
			shell: '默认终端',
			context_limits: '上下文限制',
			llm_router: '模型与媒体路由',
			session_runtime: '会话运行参数',
			mcp: 'MCP 服务',
			security: '安全策略',
			logging: '日志级别',
			hotkey: '录音快捷键',
			skills: 'Skills',
			skills_restart: 'Skills 执行配置',
			tool_settings: '工具设置',
			memory_inference: '记忆推断',
		};
		return labels[target] ?? target;
	}

	function runtimeResultLabel(result: string): string {
		if (result === 'retryable') return '可重试';
		if (result === 'pending_restart') return '重启后生效';
		return '等待应用';
	}

	/** @returns {Promise<boolean>} */
	async function saveSettings() {
		if (!settingsLoaded || !savedSnapshot || saveState === 'saving' || providerSaveBusy)
			return false;
		const securityChanged = dirtySettingsSectionIds.includes('security');
		saveState = 'saving';
		saveError = null;
		let settleSave!: () => void;
		const saveSettled = new Promise<void>((resolve) => {
			settleSave = resolve;
		});
		settingsSaveSettled = saveSettled;
		try {
			validateModelProviderBindings();
			await reconcileChatModelBeforeSave();
			await stageSettingsCredentials();
			skipNextChatModelSync = true;
			const settingsValues = {
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
					execution_mode: security.execution_mode,
					resource_policy: security.resource_policy,
					execution_root: security.execution_root,
					permissions: security.permissions,
				},
				context_limits: contextLimits,
				media: {
					input_strategy: mediaInputStrategy,
					audio: {
						max_duration_secs: audio.max_duration_secs,
						silence_timeout_ms: audio.silence_timeout_ms,
						vad_threshold: audio.vad_threshold,
					},
					stt: {
						provider_name: stt.providerName,
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
						provider_name: tts.providerName,
						model: tts.model,
						voice: tts.voice,
						timeout_secs: tts.timeout_secs,
					},
					image_gen: {
						provider_name: imageGen.providerName,
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
			};
			const baseline = JSON.parse(savedSnapshot) as Record<string, unknown>;
			if (securityChanged) {
				const latestSettings = await loadSettings();
				if (!latestSettings?.security) {
					throw new Error('无法读取最新安全设置，未保存此次安全设置变更。');
				}
				settingsValues.security = mergeChangedSettingsFields(
					baseline.security,
					settingsValues.security,
					latestSettings.security,
				) as typeof settingsValues.security;
			}
			const settingsPatch = Object.fromEntries(
				Object.entries(settingsValues).filter(
					([key, value]) => JSON.stringify(value) !== JSON.stringify(baseline[key]),
				),
			) as import('$lib/contracts/settings.ts').SettingsUpdatePayload;
			await updateSettingsCommand({ settings: settingsPatch });
			try {
				settingsApplyStatuses = await getSettingsRuntimeApplyStatus();
			} catch (statusError) {
				reportError(statusError, {
					context: 'SettingsView',
					message: '读取运行时配置状态失败',
					log: false,
				});
			}
			if (securityChanged) {
				securityRuntimeStatus = 'current';
				securityRuntimeNotice = '安全策略已按当前配置完成运行时应用。';
			}
			publishNotificationSettingsChanged(notification);
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
			const failure = presentSettingsFailure(e);
			saveError = failure;
			if (partialApplyFailure) {
				await refreshSettingsApplyStatus();
				try {
					const persistedSettings = await loadSettings();
					if (persistedSettings?.notification) {
						publishNotificationSettingsChanged(persistedSettings.notification);
					}
				} catch (refreshError) {
					reportError(refreshError, {
						context: 'SettingsView',
						message: '读取已写入的通知设置失败',
						log: false,
					});
				}
			}
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
				message: failure.title,
				log: false,
				notificationMessage: `${failure.title}：${failure.message}`,
				notificationType: failure.tone,
			});
			return false;
		} finally {
			settleSave();
			if (settingsSaveSettled === saveSettled) settingsSaveSettled = null;
		}
	}

	async function handleSaveClick() {
		if (hookDraftState.dirty && !settingsDirty) {
			await hookDraftState.save();
			return;
		}
		const action = selectSettingsSaveAction(settingsLoaded, anySettingsDirty);
		if (action === 'loading') {
			addNotification('设置仍在加载，请稍候', 'info', 2500);
			return;
		}
		if (action === 'empty') {
			addNotification('没有需要保存的设置', 'info', 2500);
			return;
		}
		await saveAllChanges();
	}

	async function saveAllChanges() {
		if (settingsDirty && (!settingsLoaded || !(await saveSettings()))) return false;
		if (hookDraftState.dirty && !(await hookDraftState.save())) return false;
		return true;
	}
</script>

<WorkspaceSurface {entering} {onAnimationEnd}>
	<div class="settings-page">
		<WorkspacePageHeader
			title="设置"
			description="按用途分组管理 Haven 配置；修改分类后可以继续浏览，离开页面时会提醒保存。"
		/>
		{#if visibleApplyStatuses.length > 0 || reapplyError}
			<section
				class="settings-runtime-apply motion-surface-enter"
				aria-labelledby="settings-runtime-apply-title"
			>
				<div class="settings-runtime-apply__content">
					<strong id="settings-runtime-apply-title">已保存配置的运行状态</strong>
					<p>
						{visibleApplyStatuses.length > 0
							? '配置已写入；以下目标尚未完全生效。'
							: '已保存配置的重新应用未完成。'}
					</p>
					{#if visibleApplyStatuses.length > 0}
						<ul class="motion-list">
							{#each visibleApplyStatuses as status (`${status.target}:${status.result}`)}
								<li>
									<div class="settings-runtime-apply__row">
										<strong>{runtimeTargetLabel(status.target)}</strong>
										<span>{runtimeResultLabel(status.result)}</span>
									</div>
									<small>
										期望版本 {status.expectedConfigVersion}；实际完整应用版本 {status.actualConfigVersion}
									</small>
									{#if status.reason}<small class="settings-runtime-apply__reason"
											>{status.reason}</small
										>{/if}
								</li>
							{/each}
						</ul>
					{/if}
					{#if reapplyError}
						<SettingsErrorNotice failure={reapplyError} />
					{/if}
				</div>
				{#if reapplyError || visibleApplyStatuses.some((status) => status.result === 'retryable' || status.result === 'pending')}
					<MaterialButton
						variant="outlined"
						label={reapplyBusy ? '重新应用中…' : '重试已保存配置'}
						onclick={reapplySavedRuntimeTargets}
						disabled={reapplyBusy || saveState === 'saving'}
					/>
				{/if}
			</section>
		{/if}

		<div class="settings-layout workspace-secondary-layout responsive-layout-transition">
			<aside class="settings-sidebar workspace-secondary-sidebar responsive-layout-panel">
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
					aria-busy={saveState === 'saving'}
					inert={saveState === 'saving'}
				>
					<WorkspaceSectionHeader
						title={activeSettingsSection.label}
						description={activeSettingsSection.description}
					>
						{#snippet children()}
							<SettingsSaveBar
								dirty={anySettingsDirty}
								dirtySectionLabels={dirtySettingsSectionLabels}
								blocked={providerSaveBusy}
								{saveState}
								{saveError}
								onDiscard={discardAndReset}
								onSave={handleSaveClick}
							/>
						{/snippet}
					</WorkspaceSectionHeader>
					<div class="settings-search-area">
						<WorkspaceSearch
							id="settings-search"
							label="搜索设置"
							value={settingsSearchQuery}
							placeholder={settingsLoaded
								? '例如：录音快捷键、API Key、上下文窗口'
								: '设置加载完成后可搜索'}
							describedBy="settings-search-help"
							controls={settingsSearchQuery.trim()
								? 'settings-search-results'
								: undefined}
							disabled={!settingsLoaded}
							onInput={(value) => void searchSettings(value)}
						/>
						<p id="settings-search-help" class="settings-search__help">
							搜索字段名称或用途；选择结果会切换分类并定位到对应设置。
						</p>
						{#if settingsSearchQuery.trim()}
							<div
								id="settings-search-results"
								class="settings-search__results motion-list"
								role="region"
								aria-label="设置搜索结果"
								aria-live="polite"
							>
								{#if settingsSearchLoading}
									<p class="settings-search__empty">正在搜索设置…</p>
								{:else if settingsSearchResults.length > 0}
									{#each settingsSearchResults as result (result.id)}
										<button
											type="button"
											class="settings-search__result"
											onclick={() => void openSettingsSearchResult(result)}
										>
											<span class="settings-search__result-copy">
												<strong>{result.label}</strong>
												{#if result.description}
													<small>{result.description}</small>
												{/if}
											</span>
											<span class="settings-search__category"
												>{result.sectionLabel}</span
											>
										</button>
									{/each}
								{:else}
									<p class="settings-search__empty">
										没有找到匹配的设置，试试“录音”“安全”或“限制”。
									</p>
								{/if}
							</div>
						{/if}
					</div>
					{#if visitedSettingsTabs.includes('behavior')}
						<div
							hidden={settingsTab !== 'behavior'}
							class:motion-surface-enter={settingsTab === 'behavior'}
							data-settings-section="behavior"
						>
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
						<div
							hidden={settingsTab !== 'appearance'}
							class:motion-surface-enter={settingsTab === 'appearance'}
							data-settings-section="appearance"
						>
							<SettingsAppearance
								{notification}
								{autostartEnabled}
								onAutostartChange={setAutostart}
							/>
						</div>
					{/if}
					{#if visitedSettingsTabs.includes('diagnostics')}
						<div
							hidden={settingsTab !== 'diagnostics'}
							class:motion-surface-enter={settingsTab === 'diagnostics'}
							data-settings-section="diagnostics"
						>
							<SettingsDiagnostics {log} />
						</div>
					{/if}
					{#if visitedSettingsTabs.includes('models') || visitedSettingsTabs.includes('media')}
						<div
							hidden={settingsTab !== 'models' && settingsTab !== 'media'}
							class:motion-surface-enter={settingsTab === 'models' ||
								settingsTab === 'media'}
							data-settings-section={settingsTab}
						>
							{#if settingsLoaded}
								<ModelSettings
									section={modelSection}
									active={settingsTab === 'models' || settingsTab === 'media'}
									settingsSaving={saveState === 'saving'}
									onProviderSaveBusy={(busy) => (providerSaveBusy = busy)}
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
									searching={Boolean(settingsSearchQuery.trim())}
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
								<LoadingState
									label="正在加载模型与 API Key 状态…"
									variant="inline"
								/>
							{/if}
						</div>
					{/if}
					{#if visitedSettingsTabs.includes('security')}
						<div
							hidden={settingsTab !== 'security'}
							class:motion-surface-enter={settingsTab === 'security'}
							data-settings-section="security"
						>
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
								onPickExecutionRoot={pickExecutionWorkspace}
							/>
						</div>
					{/if}
					{#if visitedSettingsTabs.includes('hooks')}
						<div
							hidden={settingsTab !== 'hooks'}
							class:motion-surface-enter={settingsTab === 'hooks'}
							data-settings-section="hooks"
						>
							<SettingsHooks
								active={settingsTab === 'hooks'}
								onDraftState={(draft) => (hookDraftState = draft)}
							/>
						</div>
					{/if}
					{#if visitedSettingsTabs.includes('limits')}
						<div
							hidden={settingsTab !== 'limits'}
							class:motion-surface-enter={settingsTab === 'limits'}
							data-settings-section="limits"
						>
							<SettingsLimits
								{contextLimits}
								modelContextWindows={llmConfig.models.map(
									(model) => model.context_window,
								)}
							/>
						</div>
					{/if}
					{#if visitedSettingsTabs.includes('about')}
						<div
							hidden={settingsTab !== 'about'}
							class:motion-surface-enter={settingsTab === 'about'}
							data-settings-section="about"
						>
							<SettingsAbout />
						</div>
					{/if}
				</div>
			</div>
		</div>
	</div>
</WorkspaceSurface>

<MaterialDialog open={leaveDialogOpen} title="未保存的更改" onClose={stayOnSettings}>
	{#snippet children()}
		<p>
			{saveError
				? '设置未能保存。请查看错误说明后重试，或放弃更改并离开。'
				: '设置已修改但尚未保存。你可以放弃更改并离开，或先保存再离开。'}
		</p>
		{#if saveError}
			<SettingsErrorNotice failure={saveError} />
		{/if}
	{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="text"
			label="放弃"
			ariaLabel="放弃更改并离开"
			onclick={leaveWithoutSaving}
			disabled={leaveSaving || saveState === 'saving' || providerSaveBusy}
		/>
		<MaterialButton
			variant="filled"
			label={leaveSaving ? '保存中…' : saveError ? '重试保存' : '保存'}
			ariaLabel={saveError ? '重试保存并离开' : '保存并离开'}
			onclick={leaveWithSaving}
			disabled={leaveSaving || saveState === 'saving' || providerSaveBusy}
		/>
	{/snippet}
</MaterialDialog>

<MaterialDialog
	open={!!providerDiscoveryAlert.providerName}
	title={providerDiscoveryAlert.staticCatalog ? '无法在线验证 API Key' : '模型列表获取失败'}
	onClose={() => (providerDiscoveryAlert = { providerName: '', staticCatalog: false })}
>
	{#snippet children()}
		<div class="provider-discovery-notice motion-surface-enter" role="alert">
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
	.settings-page {
		display: flex;
		flex: 1;
		flex-direction: column;
		width: 100%;
		min-width: 0;
		max-width: var(--md-sys-content-max-width);
		padding-bottom: 0;
		container-name: settings-page;
		container-type: inline-size;
	}
	.settings-page :global(.page-heading-actions) {
		flex: 0 1 auto;
		width: auto;
		min-width: 0;
		margin-left: auto;
		justify-content: flex-end;
	}
	.settings-runtime-apply {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin: 0 0 var(--md-sys-space-lg);
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
	}
	.settings-runtime-apply__content {
		min-width: 0;
		flex: 1 1 auto;
	}
	.settings-runtime-apply__content > strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-title-small-size);
	}
	.settings-runtime-apply__content > p {
		margin: var(--md-sys-space-2xs) 0 var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.settings-runtime-apply ul {
		display: grid;
		gap: var(--md-sys-space-xs);
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.settings-runtime-apply li {
		display: grid;
		gap: var(--md-sys-space-2xs);
	}
	.settings-runtime-apply__row {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	.settings-runtime-apply__row strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.settings-runtime-apply__row span,
	.settings-runtime-apply li small {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
	}
	.settings-runtime-apply__reason {
		overflow-wrap: anywhere;
	}
	.settings-search-area {
		position: relative;
		z-index: 2;
		width: 100%;
		margin-bottom: var(--md-sys-space-lg);
	}
	.settings-search__help,
	.settings-search__empty {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.settings-search__results {
		display: flex;
		flex-direction: column;
		max-height: min(280px, 38vh);
		margin-top: var(--md-sys-space-xs);
		overflow: auto;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
		box-shadow: var(--md-sys-elevation-2);
	}
	.settings-search__result {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 0;
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font: inherit;
		text-align: left;
		cursor: pointer;
	}
	.settings-search__result:last-child {
		border-bottom: 0;
	}
	.settings-search__result:hover,
	.settings-search__result:focus-visible {
		background: var(--md-sys-color-surface-container-high);
	}
	.settings-search__result:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: -2px;
	}
	.settings-search__result-copy {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-2xs);
		min-width: 0;
	}
	.settings-search__result-copy strong,
	.settings-search__result-copy small {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.settings-search__result-copy strong {
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.settings-search__result-copy small {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		white-space: nowrap;
	}
	.settings-search__category {
		flex: none;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
	}
	:global(.settings-search-target) {
		border-radius: var(--md-sys-shape-small);
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: 3px;
		background-color: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 24%,
			transparent
		);
		scroll-margin-block: 24px;
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
	@container settings-page (max-width: 640px) {
		.settings-runtime-apply {
			flex-direction: column;
		}
		.settings-page :global(.page-heading-actions) {
			flex: 1 1 100%;
			width: 100%;
		}
		.settings-page :global(.page-heading-actions .save-bar) {
			width: 100%;
		}
	}
	@media screen and (orientation: landscape) and (max-height: 560px) {
		.settings-page :global(.page-heading) {
			gap: var(--md-sys-space-sm);
			margin-bottom: var(--md-sys-space-md);
		}
	}
	@media screen and (orientation: landscape) and (max-height: 560px) and (max-width: 839.99px) {
		.settings-sidebar :global(.workspace-secondary-tabs--sidebar) {
			height: 44px;
			padding-block: var(--md-sys-space-xs);
			margin-bottom: var(--md-sys-space-sm);
		}
	}
</style>
