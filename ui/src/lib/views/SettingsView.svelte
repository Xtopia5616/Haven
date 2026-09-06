<script>
	import { onMount, onDestroy, tick } from 'svelte';
	import { invoke } from '$lib/tauri.ts';
	import { registerListeners } from '$lib/events.ts';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import { addNotification } from '$lib/stores.ts';
	import { formatError } from '$lib/formatError.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { registerSettingsLeaveGuard } from '$lib/settingsGuard.ts';
	import { resolveSettingsSaveAction } from '$lib/settingsSaveAction.ts';
	import { ensureRoleSlots } from '$lib/modelRoles.ts';
	import {
		parseApiKeyStatus,
		parseLogInfo,
		parseLogTail,
		parseShellAvailability,
	} from '$lib/contracts/settings.ts';
	import ModelSettings from './ModelSettings.svelte';
	import SettingsGeneral from './SettingsGeneral.svelte';
	import SettingsLimits from './SettingsLimits.svelte';
	import logger from '$lib/logger.ts';

	/** @type {{ providers: any[], roles: any[], [key: string]: any }} */
	let llmConfig = $state({
		providers: [],
		roles: [],
		stt_use_audio_model: true,
		vision_use_image_model: true,
		max_concurrent_requests: 2,
	});
	/** @type {{ [key: string]: boolean }} */
	let keyConfigured = $state({
		small_model: false,
		default_model: false,
		image_model: false,
		audio_model: false,
		embedding_model: false,
		stt: false,
		ocr: false,
		ocr_secret: false,
	});
	let keyConfiguredProviders = $state({});
	let hotkeyMode = $state('toggle');
	let hotkeyBinding = $state('Ctrl+Shift+Space');
	let autostartEnabled = $state(false);
	let defaultShell = $state('powershell');
	/** @type {Record<string, boolean>} */
	let shellAvailable = $state({ cmd: false, powershell: false, pwsh: false });
	let audio = $state({
		sample_rate: 16000,
		channels: 1,
		bits_per_sample: 16,
		max_duration_secs: 60,
		silence_timeout_ms: 1500,
		vad_threshold: 0.5,
	});
	let session = $state({ max_concurrent: 3, max_steps: 30 });
	/** @type {Record<string, number>} */
	let contextLimits = $state({
		compaction_ratio: 0.75,
		compaction_reserve_tokens: 4096,
		default_context_window: 128000,
		max_response_tokens: 1000000,
		max_observation_chars: 8000,
		max_transcript_chars: 4000,
		max_attachment_images: 4,
		max_attachment_files: 5,
		max_attachment_image_bytes: 10 * 1024 * 1024,
		max_attachment_file_bytes: 20 * 1024 * 1024,
		max_attachment_image_dim_px: 1568,
		attachment_image_jpeg_quality: 0.85,
		file_read_max_chars: 128000,
		file_line_span: 100,
		file_max_line_chars: 128000,
		file_summary_input_chars: 60000,
		file_max_list_entries: 1000,
		file_max_byte_read: 16 * 1024 * 1024,
		file_vision_max_bytes: 8 * 1024 * 1024,
		search_snippet_chars: 200,
		search_max_results: 1000,
		search_max_file_size_bytes: 100 * 1024 * 1024,
		search_window_bytes: 16 * 1024 * 1024,
		notification_summary_chars: 800,
		partial_checkpoint_min_chars: 1000,
		partial_checkpoint_interval_secs: 2,
		fact_infer_interval_steps: 25,
		max_known_facts: 40,
		sanitize_field_max_chars: 256,
		file_summary_timeout_secs: 120,
		cut_off_retries: 2,
		empty_response_max_retries: 3,
		empty_response_retry_delay_ms: 1500,
		stream_stall_warn_delay_ms: 10000,
		reasoning_echo_max_chars: 3000,
		background_job_tail_max_chars: 2000,
		background_job_output_emit_interval_ms: 1500,
		terminal_job_ttl_secs: 600,
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
		scheduled_actions_max: 32,
		reminders_due_horizon_secs: 365 * 24 * 3600,
		background_max_actions: 64,
		event_chunk_batch_max_bytes: 8 * 1024,
		input_ring_buffer_secs: 20,
		embedding_chunk_size: 64,
		max_tools_per_request: 128,
	});
	let memory = $state({ session_window_size: 50, history_retention_days: 90 });
	let memoryMaintenance = $state({ running: false, lastCount: null });
	/** @type {{ confirmation_mode: string, min_risk_level: string, permissions: any[] }} */
	let security = $state({ confirmation_mode: 'ask', min_risk_level: 'medium', permissions: [] });
	let stt = $state({
		provider: 'llm',
		mcp_server: '',
		model: '',
		timeout_secs: 30,
		min_confidence: 0.7,
	});
	let ocr = $state({
		provider: 'llm',
		api_key: '',
		api_secret: '',
		base_url: '',
		timeout_secs: 20,
		min_confidence: 0.7,
	});
	let tts = $state({ provider: 'none', model: '', voice: '', timeout_secs: 60 });
	let imageGen = $state({ provider: 'none', model: '', timeout_secs: 120 });
	/** @type {Record<string, any>} */
	let notification = $state({
		session_created: { in_app: true, windows: false },
		session_completed: { in_app: true, windows: true },
		session_paused: { in_app: true, windows: false },
		session_resumed: { in_app: true, windows: false },
		session_error: { in_app: true, windows: true },
	});
	let log = $state({ level: 'info', file_enabled: true });

	let settingsTab = $state('general');
	const settingsTabs = [
		{ id: 'general', label: '常规', hint: '快捷键、会话、记忆与外观' },
		{ id: 'models', label: '模型', hint: 'Provider、角色与 API Key' },
		{ id: 'media', label: '媒体', hint: '语音、图片、朗读与生成' },
		{ id: 'limits', label: '限制', hint: '上下文、文件与安全边界' },
	];
	/** @type {string[]} */
	let mcpServerNames = $state([]);
	let settingsLoaded = $state(false);
	let logView = $state({ open: false, path: '', content: '', loading: false });
	let logPreEl = /** @type {HTMLPreElement | null} */ ($state(null));
	let savedSnapshot = $state('');
	let leaveDialogOpen = $state(false);
	/** @type {((ok: boolean) => void) | null} */
	let leaveDialogResolve = null;
	let leaveSaving = $state(false);
	let saveState = $state('idle');
	let saveError = $state('');
	let mounted = true;
	/** @type {{ ready: Promise<void>, dispose: () => void } | null} */
	let eventRegistrations = null;
	let defaultModelSyncGen = 0;
	let skipNextDefaultModelSync = false;
	/** @type {{ model: string, reasoning_effort: string, web_search: string }} */
	let lastSyncedDefaultModel = { model: '', reasoning_effort: '', web_search: 'off' };

	async function checkShells() {
		for (const shell of ['cmd', 'powershell', 'pwsh']) {
			try {
				shellAvailable[shell] = !!parseShellAvailability(
					await invoke('check_shell_available', { shell }),
				)?.available;
			} catch (e) {
				shellAvailable[shell] = false;
				logger.warn('SettingsView', `check_shell_available ${shell} failed`, formatError(e));
			}
		}
	}

	async function openLogViewer() {
		logView.loading = true;
		try {
			const info = parseLogInfo(await invoke('get_log_info'));
			if (!info?.enabled) {
				addNotification('文件日志未启用，请先打开 File Logging', 'warning', 4000);
				return;
			}
			await refreshLogs();
			logView.open = true;
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '无法读取日志', log: false });
		} finally {
			logView.loading = false;
		}
	}

	async function refreshLogs() {
		try {
			const data = parseLogTail(await invoke('read_log_tail', { maxLines: 300 }));
			logView.path = data.path;
			logView.content = data.content;
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '无法读取日志', log: false });
		}
	}

	$effect(() => {
		if (logView.open && logPreEl) logPreEl.scrollTop = logPreEl.scrollHeight;
	});

	/** @param {any} remote */
	function rememberSyncedDefaultModel(remote) {
		lastSyncedDefaultModel = {
			model: remote?.model || '',
			reasoning_effort: remote?.reasoning_effort || '',
			web_search: remote?.web_search || 'off',
		};
	}
	/** @param {unknown} value */
	function asNumber(value) {
		const number = Number(value);
		return Number.isFinite(number) ? number : 0;
	}

	function buildPersistableSettings() {
		return {
			default_shell: defaultShell,
			llm: llmConfig,
			hotkey: { key_binding: hotkeyBinding, mode: hotkeyMode },
			session: {
				max_concurrent: asNumber(session.max_concurrent),
				max_steps: asNumber(session.max_steps),
			},
			memory: {
				session_window_size: asNumber(memory.session_window_size),
				history_retention_days: asNumber(memory.history_retention_days),
			},
			security: {
				confirmation_mode: security.confirmation_mode,
				min_risk_level: security.min_risk_level,
				permissions: [],
			},
			context_limits: contextLimits,
			media: {
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
			},
			log: { level: log.level, file_enabled: log.file_enabled },
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

	function discardAndReset() {
		discardChanges();
		saveState = 'idle';
		saveError = '';
	}

	/** @param {string} id */
	async function changeSettingsTab(id) {
		if (id === settingsTab) return;
		if (isDirty() && !(await confirmLeave())) return;
		settingsTab = id;
	}

	/** @param {any[]} fills */
	function reBaselineAfterDiscovery(fills) {
		if (
			!mounted ||
			!settingsLoaded ||
			!savedSnapshot ||
			!Array.isArray(fills) ||
			fills.length === 0
		)
			return;
		try {
			const snapshot = JSON.parse(savedSnapshot);
			const roles = Array.isArray(snapshot?.llm?.roles) ? snapshot.llm.roles : [];
			for (const fill of fills) {
				const role = roles.find((/** @type {any} */ item) => item.role === fill.role);
				if (!role) continue;
				if ('context_window' in fill) role.context_window = fill.context_window;
				if ('cost_per_1k_input_tokens' in fill)
					role.cost_per_1k_input_tokens = fill.cost_per_1k_input_tokens;
				if ('cost_per_1k_output_tokens' in fill)
					role.cost_per_1k_output_tokens = fill.cost_per_1k_output_tokens;
			}
			snapshot.llm = { ...(snapshot.llm || {}), roles };
			savedSnapshot = JSON.stringify(snapshot);
		} catch (e) {
			logger.warn('SettingsView', 're-baseline after discovery failed', e);
		}
	}

	/** @param {any} remote */
	function patchSnapshotDefaultModel(remote) {
		if (!savedSnapshot || !remote) return;
		try {
			const snapshot = JSON.parse(savedSnapshot);
			const roles = Array.isArray(snapshot?.llm?.roles) ? snapshot.llm.roles : [];
			const index = roles.findIndex(
				(/** @type {any} */ role) => role.role === 'default_model',
			);
			const patched = {
				...(index >= 0 ? roles[index] : { role: 'default_model' }),
				provider: remote.provider,
				model: remote.model,
				reasoning_effort: remote.reasoning_effort,
				web_search: remote.web_search,
			};
			if (index >= 0) roles[index] = patched;
			else roles.push(patched);
			snapshot.llm = { ...(snapshot.llm || {}), roles };
			savedSnapshot = JSON.stringify(snapshot);
		} catch (e) {
			logger.warn('SettingsView', 'patch snapshot default_model failed', e);
		}
	}

	/** @param {any} remote */
	function applyRemoteDefaultModelFields(remote) {
		if (!remote) return;
		const local = /** @type {any[]} */ (
			Array.isArray(llmConfig.roles) ? llmConfig.roles : []
		).find((/** @type {any} */ role) => role.role === 'default_model');
		if (local)
			Object.assign(local, {
				provider: remote.provider,
				model: remote.model,
				reasoning_effort: remote.reasoning_effort,
				web_search: remote.web_search,
			});
		else if (Array.isArray(llmConfig.roles)) llmConfig.roles.push(remote);
		rememberSyncedDefaultModel(remote);
		patchSnapshotDefaultModel(remote);
	}

	async function syncDefaultModelRoleFromBackend() {
		const generation = ++defaultModelSyncGen;
		try {
			const settings = await invoke('get_settings');
			if (!mounted || generation !== defaultModelSyncGen || !settings?.llm) return;
			const remote = /** @type {any[]} */ (
				Array.isArray(settings.llm.roles) ? settings.llm.roles : []
			).find((/** @type {any} */ role) => role.role === 'default_model');
			if (remote) applyRemoteDefaultModelFields(remote);
		} catch (e) {
			logger.warn('SettingsView', 'sync default_model role error', e);
		}
	}

	async function reconcileDefaultModelBeforeSave() {
		try {
			const settings = await invoke('get_settings');
			if (!mounted || !settings?.llm) return;
			const remote = /** @type {any[]} */ (
				Array.isArray(settings.llm.roles) ? settings.llm.roles : []
			).find((/** @type {any} */ role) => role.role === 'default_model');
			const local = /** @type {any[]} */ (
				Array.isArray(llmConfig.roles) ? llmConfig.roles : []
			).find((/** @type {any} */ role) => role.role === 'default_model');
			if (!remote || !local) return;
			if ((local.model || '') === lastSyncedDefaultModel.model) local.model = remote.model;
			if ((local.reasoning_effort || '') === lastSyncedDefaultModel.reasoning_effort)
				local.reasoning_effort = remote.reasoning_effort;
			if ((local.web_search || 'off') === lastSyncedDefaultModel.web_search)
				local.web_search = remote.web_search;
			rememberSyncedDefaultModel(local);
		} catch (e) {
			logger.warn('SettingsView', 'reconcile default_model before save failed', e);
		}
	}

	function discardChanges() {
		if (!savedSnapshot) return;
		try {
			const snapshot = JSON.parse(savedSnapshot);
			defaultShell = snapshot.default_shell || defaultShell;
			if (snapshot.llm) {
				llmConfig = {
					...llmConfig,
					...snapshot.llm,
					providers: Array.isArray(snapshot.llm.providers) ? snapshot.llm.providers : [],
					roles: Array.isArray(snapshot.llm.roles) ? snapshot.llm.roles : [],
				};
				rememberSyncedDefaultModel(
					llmConfig.roles.find((role) => role.role === 'default_model'),
				);
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
		} catch (e) {
			logger.warn('SettingsView', 'discard changes failed', e);
		}
	}

	function confirmLeave() {
		if (leaveDialogOpen)
			return new Promise((resolve) => {
				const previous = leaveDialogResolve;
				leaveDialogResolve = (ok) => {
					previous?.(false);
					resolve(ok);
				};
			});
		leaveDialogOpen = true;
		return new Promise((resolve) => {
			leaveDialogResolve = resolve;
		});
	}
	/** @param {boolean} ok */
	function finishLeaveDialog(ok) {
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
					if (skipNextDefaultModelSync) {
						skipNextDefaultModelSync = false;
						return;
					}
					syncDefaultModelRoleFromBackend();
				},
			},
			{ tag: 'SettingsView' },
		);
		try {
			const settings = await invoke('get_settings');
			if (!mounted) return;
			if (settings) {
				llmConfig = settings.llm || llmConfig;
				llmConfig.providers = Array.isArray(llmConfig.providers) ? llmConfig.providers : [];
				llmConfig.roles = Array.isArray(llmConfig.roles) ? llmConfig.roles : [];
				ensureRoleSlots(llmConfig.roles);
				rememberSyncedDefaultModel(
					llmConfig.roles.find(
						(/** @type {any} */ role) => role.role === 'default_model',
					),
				);
				hotkeyBinding = settings.hotkey?.key_binding || hotkeyBinding;
				hotkeyMode = settings.hotkey?.mode || 'toggle';
				session = settings.session || session;
				contextLimits = settings.context_limits || contextLimits;
				memory = settings.memory || memory;
				security = {
					confirmation_mode: settings.security?.confirmation_mode || 'ask',
					min_risk_level: settings.security?.min_risk_level || 'medium',
					permissions: Array.isArray(settings.security?.permissions)
						? settings.security.permissions
						: [],
				};
				const media = settings.media || {};
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
					api_key: media.ocr?.api_key || '',
					api_secret: media.ocr?.api_secret || '',
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
					.map((/** @type {any} */ server) => server.name || '')
					.filter(Boolean);
				notification = settings.notification || notification;
				log = settings.log || log;
				defaultShell = settings.default_shell || 'powershell';
				checkShells();
			}
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '加载设置失败', log: false });
		}
		// Keep the model role shape stable even when the initial settings request
		// fails. Otherwise opening the model tab would create missing role slots
		// after the baseline snapshot and incorrectly mark settings as dirty.
		if (mounted) ensureRoleSlots(llmConfig.roles);
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
			autostartEnabled = await invoke('is_autostart_enabled');
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
			memoryMaintenance.lastCount = await invoke('run_memory_maintenance');
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
		const { providers, ...flags } = parseApiKeyStatus(await invoke('get_api_key_status'));
		keyConfigured = { ...keyConfigured, ...flags };
		keyConfiguredProviders = { ...providers };
	}
	/** @param {string} key */
	async function revokePermission(key) {
		try {
			await invoke('revoke_permission', { key });
			security.permissions = security.permissions.filter(
				(/** @type {any} */ permission) => permission.key !== key,
			);
		} catch (e) {
			reportError(e, { context: 'SettingsView', message: '撤销权限失败', log: false });
		}
	}
	/** @param {string} value */
	function setHotkeyMode(value) {
		hotkeyMode = value;
	}
	/** @param {string} value */
	function setHotkeyBinding(value) {
		hotkeyBinding = value;
	}
	/** @param {string} value */
	function setDefaultShell(value) {
		defaultShell = value;
	}
	/** @param {boolean} value */
	function setAutostart(value) {
		autostartEnabled = value;
	}

	/** @returns {Promise<boolean>} */
	async function saveSettings() {
		if (saveState === 'saving') return false;
		saveState = 'saving';
		saveError = '';
		try {
			await reconcileDefaultModelBeforeSave();
			skipNextDefaultModelSync = true;
			await invoke('update_settings', {
				settings: {
					default_shell: defaultShell,
					llm: llmConfig,
					hotkey: { key_binding: hotkeyBinding, mode: hotkeyMode, mute_hotkey: null },
					session: {
						max_concurrent: session.max_concurrent,
						max_steps: session.max_steps,
					},
					memory: {
						session_window_size: memory.session_window_size,
						history_retention_days: memory.history_retention_days,
					},
					security: {
						confirmation_mode: security.confirmation_mode,
						min_risk_level: security.min_risk_level,
						permissions: [],
					},
					context_limits: contextLimits,
					media: {
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
							api_key: ocr.api_key,
							api_secret: ocr.api_secret,
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
					},
					log: { level: log.level, file_enabled: log.file_enabled, file_path: null },
				},
			});
			addNotification('设置已保存', 'success');
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
				if (autostartEnabled) await invoke('enable_autostart');
				else await invoke('disable_autostart');
			} catch (e) {
				autostartEnabled = !autostartEnabled;
				addNotification(`自动启动：${formatError(e)}`, 'warning');
			}
			if (mounted) captureSnapshot();
			saveState = 'saved';
			return true;
		} catch (e) {
			skipNextDefaultModelSync = false;
			saveState = 'error';
			saveError = formatError(e);
			reportError(e, { context: 'SettingsView', message: '保存设置失败', log: false });
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
</script>

<div class="settings-page">
	<div class="page-heading">
		<div class="page-heading-content">
			<h1>设置</h1>
			<p>调整 Haven 的模型、语音、性能与安全行为。</p>
		</div>
	</div>
	{#if settingsLoaded && settingsTab !== 'models' && llmConfig.providers.length === 0}
		<div class="settings-callout" data-state="unconfigured" role="status">
			<div>
				<strong>模型尚未配置</strong>
				<p>
					添加 Provider 后，Haven 才能生成回复。你可以先完成模型配置，再回来调整其他选项。
				</p>
			</div>
			<MaterialButton
				variant="outlined"
				label="去配置模型"
				onclick={() => changeSettingsTab('models')}
			/>
		</div>
	{/if}
	<div class="md-tabs settings-tabs" role="tablist">
		{#each settingsTabs as tab}<button
				class="md-tab"
				class:active={settingsTab === tab.id}
				role="tab"
				aria-controls="settings-panel"
				aria-selected={settingsTab === tab.id}
				onclick={() => changeSettingsTab(tab.id)}
			>
				<span>{tab.label}</span>
				<small>{tab.hint}</small>
			</button>{/each}
	</div>
	<div
		id="settings-panel"
		role="tabpanel"
		aria-label={settingsTabs.find((tab) => tab.id === settingsTab)?.label || '设置'}
	>
		{#if settingsTab === 'general'}
			<SettingsGeneral
				{hotkeyMode}
				{hotkeyBinding}
				{llmConfig}
				{session}
				{defaultShell}
				{shellAvailable}
				{memory}
				{memoryMaintenance}
				{security}
				{notification}
				{log}
				{logView}
				{autostartEnabled}
				onHotkeyModeChange={setHotkeyMode}
				onHotkeyBindingChange={setHotkeyBinding}
				onDefaultShellChange={setDefaultShell}
				onAutostartChange={setAutostart}
				onRunMaintenance={runMaintenance}
				onOpenLogViewer={openLogViewer}
				onRevokePermission={revokePermission}
			/>
		{:else if settingsTab === 'models' || settingsTab === 'media'}
			{#if settingsLoaded}<ModelSettings
					section={settingsTab}
					{llmConfig}
					{audio}
					{stt}
					{ocr}
					{tts}
					{imageGen}
					{contextLimits}
					{keyConfigured}
					{keyConfiguredProviders}
					{mcpServerNames}
					loaded={true}
					onDiscoverySettled={reBaselineAfterDiscovery}
				/>{:else}<p class="model-hint">正在加载模型与 API Key 状态…</p>{/if}
		{:else}
			<SettingsLimits {contextLimits} />
		{/if}
	</div>
	{#if settingsDirty}
		<div class="save-bar md-toolbar">
			<div class="save-actions">
				<MaterialButton
					variant="outlined"
					label="放弃更改"
					onclick={discardAndReset}
					disabled={saveState === 'saving'}
				/>
				<div
					class="save-button-status"
					aria-live="polite"
					aria-busy={saveState === 'saving'}
				>
					<MaterialButton
						variant="filled"
						className="save-btn save-btn--dirty"
						label={saveState === 'saving' ? '保存中…' : '保存设置'}
						onclick={handleSaveClick}
						disabled={saveState === 'saving'}
					/>
				</div>
			</div>
		</div>
	{/if}
</div>

{#if logView.open}
	<MaterialDialog
		open={true}
		title="日志查看"
		dialogClass="md-dialog--wide"
		onClose={() => {
			logView.open = false;
		}}
	>
		{#snippet children()}{#if logView.path}<p class="log-path" title={logView.path}>
					{logView.path}
				</p>{/if}
			<pre class="log-viewer" bind:this={logPreEl}>{logView.content ||
					'（暂无日志内容）'}</pre>{/snippet}
		{#snippet footer()}
			<MaterialButton
				variant="outlined"
				label="刷新"
				onclick={refreshLogs}
				disabled={logView.loading}
			/>
			<MaterialButton
				variant="text"
				label="关闭"
				onclick={() => {
					logView.open = false;
				}}
			/>
		{/snippet}
	</MaterialDialog>
{/if}
<MaterialDialog open={leaveDialogOpen} title="未保存的更改" onClose={stayOnSettings}>
	{#snippet children()}<p>
			设置已修改但尚未保存。选择「取消」将放弃更改并离开；或先保存再离开。
		</p>{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="text"
			label="取消"
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

<style>
	.settings-page {
		width: 100%;
		min-width: 0;
		max-width: var(--md-sys-content-max-width);
		padding-bottom: var(--md-sys-space-xl);
	}
	.settings-tabs {
		margin-bottom: var(--md-sys-space-2xl);
	}
	.settings-tabs .md-tab {
		display: grid;
		justify-items: start;
		gap: var(--md-sys-space-xs);
		min-width: 132px;
		min-height: var(--md-comp-button-height);
		padding: var(--md-sys-space-sm) var(--md-sys-space-lg);
		text-align: left;
	}
	.settings-tabs .md-tab small {
		max-width: 180px;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 400;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.settings-callout {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-xl);
		padding: var(--md-sys-space-lg);
		border: 1px solid var(--md-sys-color-tertiary);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-tertiary-container);
		color: var(--md-sys-color-on-tertiary-container);
	}
	.settings-callout p {
		margin-top: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-tertiary-container);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.save-bar {
		position: sticky;
		bottom: 0;
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: var(--md-comp-toolbar-gap);
		margin-top: var(--md-sys-space-xl);
		padding: var(--md-sys-space-sm) 0;
		background: transparent;
		border-top: none;
		z-index: 1;
	}
	:global(.save-btn) {
		width: 96px;
		min-width: 96px;
	}
	:global(.save-btn--dirty) {
		box-shadow: var(--md-sys-elevation-2);
	}
	.save-button-status {
		display: inline-flex;
	}
	.save-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 0 0 auto;
	}
	:global(.md-dialog--wide) {
		width: min(760px, 92vw);
	}
	.log-path {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin: 0 0 var(--md-sys-space-sm);
		word-break: break-all;
	}
	.log-viewer {
		box-sizing: border-box;
		max-height: 60vh;
		overflow: auto;
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		border: 1px solid var(--md-sys-color-outline-variant);
		margin: 0;
		white-space: pre;
	}
	@media (max-width: 640px) {
		.settings-tabs .md-tab {
			min-width: 0;
			flex: 1 1 50%;
			padding-inline: var(--md-sys-space-md);
		}
		.settings-tabs .md-tab small {
			display: none;
		}
		.settings-callout,
		.save-bar {
			align-items: stretch;
			flex-direction: column;
		}
		.settings-callout :global(.md-btn),
		.save-actions,
		.save-actions :global(.md-btn),
		.save-button-status {
			width: 100%;
		}
		.save-actions {
			align-items: stretch;
		}
	}
</style>
