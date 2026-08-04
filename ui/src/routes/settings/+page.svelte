<script>
	import logger from '$lib/logger.js';
			import { onMount, onDestroy } from 'svelte';
	import { invoke } from '$lib/tauri.js';
	import { themeStore } from '$lib/themeStore.js';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import StatusDot from '$lib/StatusDot.svelte';
	import HotkeyInput from '$lib/HotkeyInput.svelte';
	import { addNotification } from '$lib/stores.js';

	let llmConfig = $state({
		small_model: { provider: 'openai', api_style: '', model_name: 'gpt-4o-mini', temperature: 0, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0 },
		default_model: { provider: 'anthropic', api_style: '', model_name: 'claude-sonnet-4-20250514', temperature: 0.7, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0 },
		balanced_model: { provider: 'local', api_style: '', model_name: 'llama3', temperature: 0.7, base_url: 'http://localhost:11434', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0 },
		image_model: { provider: 'openai', api_style: '', model_name: 'gpt-4o', temperature: 0.2, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0 },
		audio_model: { provider: 'openai', api_style: '', model_name: 'gpt-4o-audio-preview', temperature: 0, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0 },
		stt_use_audio_model: true,
		vision_use_image_model: true,
	});

	let keyConfigured = $state({
		small_model: false,
		default_model: false,
		balanced_model: false,
		image_model: false,
		audio_model: false,
		stt: false,
	});

	// Single source of truth for the LLM endpoint cards; adding a model role
	// here renders its card without duplicating markup. Cards are grouped:
	// core models (agent loop) first, then specialized (vision / speech).
	const modelCards = [
		{ key: 'default_model', label: 'Default Model', hint: 'Primary reasoning & tool-use agent', prefix: 'dm', basePlaceholder: 'https://api.openai.com/v1', group: 'core' },
		{ key: 'balanced_model', label: 'Balanced Model', hint: 'Used when Default Model is unavailable', prefix: 'bm', basePlaceholder: 'http://localhost:11434', group: 'core' },
		{ key: 'small_model', label: 'Small Model', hint: 'Title generation & lightweight reasoning', prefix: 'sm', basePlaceholder: 'https://api.openai.com/v1', group: 'core' },
		{ key: 'image_model', label: 'Image Model', hint: 'Image understanding (vision-capable)', prefix: 'im', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
		{ key: 'audio_model', label: 'Audio Model', hint: 'Audio transcription (speech-to-text)', prefix: 'au', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
	];
	const coreModelCards = modelCards.filter((c) => c.group === 'core');
	const specializedModelCards = modelCards.filter((c) => c.group === 'specialized');

	// Per-card model discovery: fetched model IDs from the provider's
	// `/models` endpoint, shown as autocomplete options on the Model field.
	// `stt` holds STT-provider model lists fetched with the STT key.
	let modelsByKey = $state({ stt: [] });
	let modelFetching = $state({ stt: false });
	let fetchTimers = {};
	// Timestamp of the last fetch notification per card, so bursts of
	// auto-fetch (focus / typing) don't spam the same message.
	let lastFetchNotify = $state({ stt: 0 });

	// Default base URL per API style, filled in when the user picks a style
	// and the Base URL field is still empty (never overwrites a custom URL).
	const styleDefaultBaseUrl = {
		'openai-chat': 'https://api.openai.com/v1',
		'openai-responses': 'https://api.openai.com/v1',
		'anthropic': 'https://api.anthropic.com',
		'gemini': 'https://generativelanguage.googleapis.com',
		'llama.cpp': 'http://127.0.0.1:8080',
	};

	const OPENAI_COMPAT_STT = new Set(['openai', 'groq']);
	const GEMINI_STT = new Set(['gemini']);

	// STT provider choices offered inside the Audio Model card's Provider
	// selector (values are prefixed `stt:` so they never collide with LLM
	// wire-protocol styles like `openai-chat`). The `openai-chat` LLM style
	// doubles as the "transcribe via the audio_model LLM" mode; Gemini audio
	// transcription is covered by the `stt:gemini` option, so no separate
	// LLM `gemini` style is offered for this slot.
	const STT_STYLE_OPTIONS = [
		{ value: 'stt:openai', label: 'OpenAI Whisper' },
		{ value: 'stt:groq', label: 'Groq' },
		{ value: 'stt:gemini', label: 'Google Gemini' },
		{ value: 'stt:deepgram', label: 'Deepgram' },
		{ value: 'stt:assemblyai', label: 'AssemblyAI' },
		{ value: 'stt:mcp', label: 'MCP Server' },
		{ value: 'stt:none', label: 'None' },
	];
	const STT_STYLE_SET = new Set(STT_STYLE_OPTIONS.map((o) => o.value));

	function sttProviderFromStyle(style) {
		return style.startsWith('stt:') ? style.slice(4) : null;
	}

	// Current value of the Audio Model card's API Style selector: either an LLM
	// wire-protocol style (`auto` / `openai-chat` / …) or an `stt:*` provider.
	// Stored separately from `llmConfig.audio_model.api_style` so switching to
	// an STT provider never clobbers the endpoint's LLM wire protocol.
	let audioApiStyle = $state('auto');

	function isOpenAiCompatibleStt(provider) {
		return OPENAI_COMPAT_STT.has(provider);
	}

	function isGeminiStt(provider) {
		return GEMINI_STT.has(provider);
	}

	// True when the Audio Model card's API Style is set to an STT provider.
	// In this mode the card's API Key / Model / Base URL fields bind to the
	// STT config instead of the audio_model endpoint, so credentials are
	// entered once and never duplicated in a separate STT section.
	function isAudioSttMode() {
		return STT_STYLE_SET.has(audioApiStyle);
	}

	function audioSttProvider() {
		return sttProviderFromStyle(audioApiStyle) || stt.provider;
	}

	function isCloudSttProvider(provider) {
		return ['openai', 'groq', 'gemini', 'deepgram', 'assemblyai'].includes(provider);
	}

	function sttModelPlaceholder(provider) {
		if (provider === 'deepgram') return 'nova-3';
		if (provider === 'assemblyai') return 'assemblyai_default';
		if (provider === 'groq') return 'whisper-large-v3-turbo';
		if (isGeminiStt(provider)) return 'gemini-2.5-flash';
		return 'whisper-1';
	}

	function sttBasePlaceholder(provider) {
		return isGeminiStt(provider) ? 'https://generativelanguage.googleapis.com/v1beta' : 'https://api.openai.com/v1';
	}

	// Model-list discovery for STT providers. OpenAI-compatible hosts expose
	// a `/models` endpoint (fetched like the LLM cards); Deepgram ships fixed
	// model ids; AssemblyAI has no public model list.
	function sttFetchBaseUrl(provider) {
		if (stt.base_url.trim()) return stt.base_url.trim();
		if (provider === 'groq') return 'https://api.groq.com/openai/v1';
		if (provider === 'gemini') return 'https://generativelanguage.googleapis.com/v1beta';
		return 'https://api.openai.com/v1';
	}

	function sttModelOptions(provider) {
		if (provider === 'deepgram') {
			return [
				{ value: 'nova-3', label: 'nova-3' },
				{ value: 'nova-2', label: 'nova-2' },
				{ value: 'whisper-large-v3', label: 'whisper-large-v3' },
				{ value: 'whisper-large-v3-turbo', label: 'whisper-large-v3-turbo' },
			];
		}
		if (provider === 'assemblyai') {
			return [
				{ value: 'assemblyai_default', label: 'AssemblyAI Default' },
				{ value: 'universal', label: 'universal' },
				{ value: 'universal-2', label: 'universal-2' },
				{ value: 'universal-3-pro', label: 'universal-3-pro' },
			];
		}
		return (modelsByKey.stt || []).map((m) => ({ value: m.id, label: m.name || m.id }));
	}

	async function fetchSttModels() {
		const provider = audioSttProvider();
		if (provider === 'deepgram' || provider === 'assemblyai' || provider === 'mcp' || provider === 'none') {
			return; // fixed lists or nothing to fetch
		}
		const base = sttFetchBaseUrl(provider);
		if (!base || (!stt.api_key && !keyConfigured.stt)) {
			modelsByKey.stt = [];
			return;
		}
		modelFetching.stt = true;
		try {
			const list = await invoke('discover_models', { baseUrl: base, apiKey: stt.api_key, role: 'stt' });
			const prev = JSON.stringify(modelsByKey.stt || []);
			modelsByKey.stt = list || [];
			if (JSON.stringify(modelsByKey.stt) !== prev) {
				notifyFetch('stt', `已获取 ${(list || []).length} 个模型`, 'success', 2500);
			}
		} catch (e) {
			const msg = typeof e === 'string' ? e : (e?.message || String(e));
			modelsByKey.stt = [];
			notifyFetch('stt', `获取模型失败: ${msg}`, 'error', 4000);
		} finally {
			modelFetching.stt = false;
		}
	}

	function scheduleSttFetch() {
		clearTimeout(fetchTimers.stt);
		fetchTimers.stt = setTimeout(() => fetchSttModels(), 600);
	}

	function onApiStyleChange(key, v) {
		if (key === 'audio_model') {
			if (STT_STYLE_SET.has(v)) {
				// STT provider picked: sync the STT config; the endpoint's LLM
				// api_style stays untouched so switching back to `openai-chat`
				// restores the previous wire protocol. Stale model-list caches
				// from the previous provider are dropped.
				audioApiStyle = v;
				const provider = sttProviderFromStyle(v);
				if (provider) stt.provider = provider;
				modelsByKey.stt = [];
				return;
			}
			// LLM style picked (openai-chat): transcription routes through
			// the audio_model endpoint, so there is no separate "LLM Adapter"
			// STT option. The endpoint provider is fixed to the wire protocol.
			if (v !== 'auto') {
				stt.provider = 'llm';
				llmConfig.audio_model.provider = 'openai';
			}
			audioApiStyle = v;
		}
		llmConfig[key].api_style = v === 'auto' ? '' : v;
		const url = styleDefaultBaseUrl[v];
		if (url && !llmConfig[key].base_url.trim()) {
			llmConfig[key].base_url = url;
		}
	}

	function notifyFetch(key, message, type, duration) {
		const now = Date.now();
		if (now - (lastFetchNotify[key] || 0) < 2500) return;
		lastFetchNotify[key] = now;
		addNotification(message, type, duration);
	}

	async function fetchModels(key) {
		const ep = llmConfig[key];
		// Stored keys are masked: when the slot is already configured the
		// backend falls back to the stored key via the role name.
		if (!ep || !ep.base_url || (!ep.api_key && !keyConfigured[key])) {
			modelsByKey[key] = [];
			return;
		}
		modelFetching[key] = true;
		try {
			const list = await invoke('discover_models', {
				baseUrl: ep.base_url,
				apiKey: ep.api_key,
				role: key,
			});
			const prev = modelsByKey[key] || [];
			const next = list || [];
			modelsByKey[key] = next;
			logger.debug('settings', `discovered ${next.length} models for ${key}`);
			if (JSON.stringify(prev) !== JSON.stringify(next)) {
				notifyFetch(key, `已获取 ${next.length} 个模型`, 'success', 2500);
			}
		} catch (e) {
			const msg = typeof e === 'string' ? e : (e?.message || String(e));
			modelsByKey[key] = [];
			notifyFetch(key, `获取模型失败: ${msg}`, 'error', 4000);
		} finally {
			modelFetching[key] = false;
		}
	}

	function scheduleFetch(key) {
		clearTimeout(fetchTimers[key]);
		fetchTimers[key] = setTimeout(() => fetchModels(key), 600);
	}

	let hotkeyMode = $state('toggle');
	let hotkeyBinding = $state('Ctrl+Shift+Space');
	let autostartEnabled = $state(false);

	let audio = $state({
		sample_rate: 16000,
		channels: 1,
		bits_per_sample: 16,
		max_duration_secs: 60,
		silence_timeout_ms: 1500,
		vad_threshold: 0.5,
	});

	let task = $state({ max_concurrent: 3, max_steps: 30 });
	let memory = $state({ session_window_size: 50, history_retention_days: 90 });
	let security = $state({ confirmation_mode: 'always', min_risk_level: 'low' });

	let stt = $state({
		provider: 'mcp',
		mcp_server: '',
		api_key: '',
		model: '',
		base_url: '',
		timeout_secs: 30,
	});
	let notification = $state({
		task_created: { in_app: true, windows: false },
		task_completed: { in_app: true, windows: true },
		task_paused: { in_app: true, windows: false },
		task_error: { in_app: true, windows: true },
	});
	let log = $state({ level: 'info', file_enabled: true });

	let preferences = $state([]);
	let prefLoaded = $state(false);
	// Names of configured MCP servers, offered in the Audio Model card's
	// Model field when the STT provider is an MCP server.
	let mcpServerNames = $state([]);

	let keyChangeDialog = $state({ open: false, model: '', label: '' });
	let newKeyValue = $state('');
	let accent = $state(themeStore.currentAccent);
	let customAccentHex = $state(themeStore.isPreset ? '#2C5090' : themeStore.accentColor);
	let savedAccent = themeStore.currentAccent; // snapshot for reverting unsaved changes
	let currentTheme = $state(themeStore.currentTheme);
	const unsubTheme = themeStore.subscribe((v) => { currentTheme = v.theme; });
	// L12: guards against onDestroy running while onMount's async settings
	// load is still in flight.
	let mounted = true;

	onDestroy(() => {
		mounted = false;
		unsubTheme();
		if (accent !== savedAccent) {
			themeStore.setAccent(savedAccent);
		}
	});

	onMount(async () => {
		try {
			const settings = await invoke('get_settings');
			if (!mounted) return;
			if (settings) {
				llmConfig = settings.llm || llmConfig;
				hotkeyBinding = settings.hotkey?.key_binding || hotkeyBinding;
				hotkeyMode = settings.hotkey?.mode || 'toggle';
				audio = settings.audio || audio;
				task = settings.task || task;
				memory = settings.memory || memory;
				security = {
					confirmation_mode: settings.security?.confirmation_mode || 'always',
					min_risk_level: settings.security?.min_risk_level || 'low',
				};
				stt = {
					provider: settings.stt?.provider || 'mcp',
					mcp_server: settings.stt?.mcp_server || '',
					api_key: settings.stt?.api_key || '',
					model: settings.stt?.model || '',
					base_url: settings.stt?.base_url || '',
					timeout_secs: settings.stt?.timeout_secs || 30,
				};
				// Audio Model Provider selector reflects the STT provider when
				// one is explicitly configured; `llm` maps back to the
				// endpoint's LLM wire style (the "transcribe via audio_model"
				// mode). A legacy `gemini` wire style is folded into the
				// `stt:gemini` option — Gemini audio transcription is a
				// single path now.
				const sttCfg = settings.stt;
				let llmAudioStyle = settings.llm?.audio_model?.api_style || '';
				if (llmAudioStyle === 'gemini') {
					audioApiStyle = 'stt:gemini';
					if (!sttCfg || sttCfg.provider === 'mcp') stt.provider = 'gemini';
				} else if (sttCfg?.provider === 'llm') {
					audioApiStyle = llmAudioStyle || 'openai-chat';
				} else if (sttCfg && (sttCfg.provider !== 'mcp' || sttCfg.mcp_server || sttCfg.api_key || sttCfg.model || sttCfg.base_url)) {
					audioApiStyle = `stt:${sttCfg.provider}`;
				} else {
					audioApiStyle = llmAudioStyle || 'auto';
				}
				mcpServerNames = (settings.mcp_servers || []).map((s) => s.name || '').filter(Boolean);
			notification = settings.notification || notification;
			log = settings.log || log;
			if (settings.appearance?.accent_color) {
				accent = settings.appearance.accent_color;
				savedAccent = accent;
				themeStore.setAccent(accent);
				if (!themeStore.presets[accent]) customAccentHex = themeStore.accentColor;
			} else {
				savedAccent = themeStore.currentAccent;
			}
			}
		} catch (e) {
			addNotification(`加载设置失败: ${e}`, 'error', 4000);
		}
		try {
			keyConfigured = await invoke('get_api_key_status');
			if (!mounted) return;
		} catch (e) {
			addNotification(`获取 API Key 状态失败: ${e}`, 'error', 3000);
		}
		try {
			autostartEnabled = await invoke('is_autostart_enabled');
			if (!mounted) return;
		} catch (e) {
			addNotification(`获取开机自启状态失败: ${e}`, 'error', 3000);
		}
		await loadPreferences();
	});

	async function loadPreferences() {
		try {
			const all = await invoke('list_preferences');
			// filter out tool_usage counters (internal)
			preferences = (all || []).filter(([k]) => !k.startsWith('tool_usage.') && !k.startsWith('tool_param.') && !k.startsWith('cfg.'));
			prefLoaded = true;
		} catch {
			preferences = [];
			logger.warn('settings', 'load preferences error');
		}
	}

	async function deletePrefKey(key) {
		try {
			await invoke('delete_preference', { key });
			preferences = preferences.filter(([k]) => k !== key);
		} catch (e) {
			addNotification(`删除偏好失败: ${e}`, 'error', 3000);
		}
	}

	async function saveSettings() {
		try {
			await invoke('update_settings', {
				settings: {
					llm: llmConfig,
					hotkey: { key_binding: hotkeyBinding, mode: hotkeyMode, mute_hotkey: null },
					audio: {
						sample_rate: audio.sample_rate,
						channels: audio.channels,
						bits_per_sample: audio.bits_per_sample,
						max_duration_secs: audio.max_duration_secs,
						silence_timeout_ms: audio.silence_timeout_ms,
						vad_threshold: audio.vad_threshold,
					},
				task: {
					max_concurrent: task.max_concurrent,
					max_steps: task.max_steps,
				},
					memory: {
						session_window_size: memory.session_window_size,
						history_retention_days: memory.history_retention_days,
					},
				security: {
					confirmation_mode: security.confirmation_mode,
					min_risk_level: security.min_risk_level,
					encrypt_sensitive: true,
				},
					stt: {
						provider: stt.provider,
						mcp_server: stt.mcp_server || null,
						api_key: stt.api_key,
						model: stt.model,
						base_url: stt.base_url,
						timeout_secs: stt.timeout_secs,
					},
					notification: {
						task_created: { in_app: notification.task_created.in_app, windows: notification.task_created.windows },
						task_completed: { in_app: notification.task_completed.in_app, windows: notification.task_completed.windows },
						task_paused: { in_app: notification.task_paused.in_app, windows: notification.task_paused.windows },
						task_error: { in_app: notification.task_error.in_app, windows: notification.task_error.windows },
					},
			log: {
				level: log.level,
				file_enabled: log.file_enabled,
				file_path: null,
			},
			appearance: {
				accent_color: accent,
			},
				},
			});
		addNotification('设置已保存', 'success');
			savedAccent = accent;
			if (autostartEnabled) {
				try { await invoke('enable_autostart'); } catch (e) {
					autostartEnabled = false;
					addNotification(`自动启动：${e}`, 'warning');
				}
			} else {
				try { await invoke('disable_autostart'); } catch (e) {
					autostartEnabled = true;
					addNotification(`取消自动启动：${e}`, 'warning');
				}
			}
		} catch (e) {
			addNotification(`保存设置失败: ${e}`, 'error', 5000);
		}
	}

	async function toggleAutostart() {
		autostartEnabled = !autostartEnabled;
	}

	function openKeyDialog(model, label) {
		keyChangeDialog = { open: true, model, label };
		newKeyValue = '';
	}

	function confirmKeyChange() {
		if (newKeyValue.trim()) {
			if (keyChangeDialog.model === 'stt') {
				stt.api_key = newKeyValue.trim();
			} else {
				llmConfig[keyChangeDialog.model].api_key = newKeyValue.trim();
				scheduleFetch(keyChangeDialog.model);
			}
			keyConfigured[keyChangeDialog.model] = true;
		}
		keyChangeDialog = { open: false, model: '', label: '' };
		newKeyValue = '';
	}

	function contrastText(hex) {
		const r = parseInt(hex.slice(1, 3), 16);
		const g = parseInt(hex.slice(3, 5), 16);
		const b = parseInt(hex.slice(5, 7), 16);
		const lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
		return lum > 0.5 ? '#000000' : '#ffffff';
	}
</script>

<div class="settings-page">
	<h1>Settings</h1>

	<div class="section">
		<h2>LLM Configuration</h2>

		{#snippet modelCard(card)}
		<div class="model-card">
			<h3>{card.label}</h3>
			<p class="model-hint">{card.hint}</p>
			{#if card.key !== 'audio_model'}
				<div class="form-row">
					<label for="{card.prefix}-provider">Provider</label>
					<input id="{card.prefix}-provider" type="text" class="md-input" bind:value={llmConfig[card.key].provider} autocomplete="off" />
				</div>
			{/if}
			<div class="form-row">
				<label for="{card.prefix}-api-style">{card.key === 'audio_model' ? 'Provider' : 'API Style'}</label>
				<MaterialSelect
					id="{card.prefix}-api-style"
					value={card.key === 'audio_model' ? audioApiStyle : (llmConfig[card.key].api_style || 'auto')}
					options={card.key === 'audio_model'
						? [
							// Only wire protocols that accept audio input; the
							// unsupported ones (anthropic, openai-responses)
							// are omitted for this slot, as is the `gemini`
							// LLM style — Gemini audio transcription is the
							// `stt:gemini` option below.
							{ value: 'auto', label: 'Auto (from provider)' },
							{ value: 'openai-chat', label: 'OpenAI Chat Completions' },
							...STT_STYLE_OPTIONS,
						]
						: [
							{ value: 'auto', label: 'Auto (from provider)' },
							{ value: 'openai-chat', label: 'OpenAI Chat Completions' },
							{ value: 'llama.cpp', label: 'llama.cpp server' },
							{ value: 'openai-responses', label: 'OpenAI Responses API' },
							{ value: 'anthropic', label: 'Anthropic (Claude)' },
							{ value: 'gemini', label: 'Google Gemini' },
						]}
					onChange={(v) => onApiStyleChange(card.key, v)}
				/>
			</div>
			{#if card.key === 'audio_model' && isAudioSttMode() && (isOpenAiCompatibleStt(audioSttProvider()) || isGeminiStt(audioSttProvider()))}
				<div class="form-row">
					<label for="{card.prefix}-base-url">Base URL</label>
					<input id="{card.prefix}-base-url" type="text" class="md-input" bind:value={stt.base_url} placeholder={sttBasePlaceholder(audioSttProvider())} autocomplete="off" />
				</div>
			{:else}
				<div class="form-row">
					<label for="{card.prefix}-base-url">Base URL</label>
					<input id="{card.prefix}-base-url" type="text" class="md-input" bind:value={llmConfig[card.key].base_url} placeholder={card.basePlaceholder} oninput={() => scheduleFetch(card.key)} autocomplete="off" />
				</div>
			{/if}
			{#if card.key === 'audio_model' && isAudioSttMode() && isCloudSttProvider(audioSttProvider())}
				<div class="form-row">
					<label for="{card.prefix}-api-key">API Key</label>
					<div class="key-status-row" class:key-not-configured={!keyConfigured.stt}>
						<StatusDot color={keyConfigured.stt ? 'success' : 'outline'} />
						<span class="key-configured-label">{keyConfigured.stt ? 'Configured' : 'Not Configured'}</span>
						<button
							id="{card.prefix}-api-key"
							class="md-btn md-btn--xs md-btn--outlined"
							onclick={() => openKeyDialog('stt', 'STT API Key')}
						>
							{keyConfigured.stt ? 'Change' : 'Set'}
						</button>
					</div>
				</div>
			{:else}
				<div class="form-row">
					<label for="{card.prefix}-api-key">API Key</label>
					<div class="key-status-row" class:key-not-configured={!keyConfigured[card.key]}>
						<StatusDot color={keyConfigured[card.key] ? 'success' : 'outline'} />
						<span class="key-configured-label">{keyConfigured[card.key] ? 'Configured' : 'Not Configured'}</span>
						<button
							id="{card.prefix}-api-key"
							class="md-btn md-btn--xs md-btn--outlined"
							onclick={() => openKeyDialog(card.key, card.label)}
						>
							{keyConfigured[card.key] ? 'Change' : 'Set'}
						</button>
					</div>
				</div>
			{/if}
			{#if card.key === 'audio_model' && isAudioSttMode()}
				{#if audioSttProvider() === 'mcp'}
					<div class="form-row">
						<label for="{card.prefix}-model">MCP Server</label>
						<div class="model-input-row">
							<MaterialAutocomplete
								id="{card.prefix}-model"
								value={stt.mcp_server}
								options={mcpServerNames.map((n) => ({ value: n, label: n }))}
								placeholder="Pick a configured MCP server"
								loading={false}
								onChange={(v) => { stt.mcp_server = v; }}
							/>
						</div>
					</div>
				{:else if isCloudSttProvider(audioSttProvider())}
					<div class="form-row">
						<label for="{card.prefix}-model">Model</label>
						<div class="model-input-row">
							<MaterialAutocomplete
								id="{card.prefix}-model"
								value={stt.model}
								options={sttModelOptions(audioSttProvider())}
								placeholder={sttModelPlaceholder(audioSttProvider())}
								loading={modelFetching.stt}
								onChange={(v) => { stt.model = v; }}
								onFocus={() => scheduleSttFetch()}
							/>
						</div>
					</div>
				{/if}
			{:else}
				<div class="form-row">
					<label for="{card.prefix}-model">Model</label>
					<div class="model-input-row">
						<MaterialAutocomplete
							id="{card.prefix}-model"
							value={llmConfig[card.key].model_name}
							options={(modelsByKey[card.key] || []).map((m) => ({ value: m.id, label: m.name || m.id }))}
							placeholder="Type or pick from fetched models"
							loading={modelFetching[card.key]}
							onChange={(v) => { llmConfig[card.key].model_name = v; }}
							onFocus={() => scheduleFetch(card.key)}
						/>
					</div>
				</div>
			{/if}
		<div class="form-row">
			<label for="{card.prefix}-temp">Temperature</label>
			<MaterialNumberField id="{card.prefix}-temp" value={llmConfig[card.key].temperature} step={0.1} min={0} max={2} onChange={(v) => { llmConfig[card.key].temperature = v; }} />
		</div>
		<div class="form-row cost-row">
			<label for="{card.prefix}-cost-in">Cost In ($/1K)</label>
			<MaterialNumberField id="{card.prefix}-cost-in" value={llmConfig[card.key].cost_per_1k_input_tokens ?? 0} step={0.01} min={0} onChange={(v) => { llmConfig[card.key].cost_per_1k_input_tokens = v; }} />
			<span class="cost-sep">/</span>
			<label for="{card.prefix}-cost-out">Out ($/1K)</label>
			<MaterialNumberField id="{card.prefix}-cost-out" value={llmConfig[card.key].cost_per_1k_output_tokens ?? 0} step={0.01} min={0} onChange={(v) => { llmConfig[card.key].cost_per_1k_output_tokens = v; }} />
		</div>
		<p class="cost-hint">USD per 1K tokens (input/output). Leave 0 to disable cost display for this model.</p>
	</div>
	{/snippet}

		<h3 class="model-group-heading">Core Models</h3>
		<div class="model-grid">
			{#each coreModelCards as card}
				{@render modelCard(card)}
			{/each}
		</div>

		<h3 class="model-group-heading">Specialized Models</h3>
		<div class="model-grid">
			{#each specializedModelCards as card}
				{@render modelCard(card)}
			{/each}
		</div>

		<h3 class="routing-heading">Model Routing</h3>
		<div class="form-row switch-row">
			<span class="switch-label">Recording transcription uses the dedicated audio model</span>
			<MaterialSwitch checked={llmConfig.stt_use_audio_model} onChange={(v) => { llmConfig.stt_use_audio_model = v; }} />
		</div>
		<div class="form-row switch-row">
			<span class="switch-label">Image understanding uses the dedicated image model</span>
			<MaterialSwitch checked={llmConfig.vision_use_image_model} onChange={(v) => { llmConfig.vision_use_image_model = v; }} />
		</div>
		<p class="model-hint">Turn off to route recording transcription and image understanding through the Default Model instead.</p>
	</div>

	<div class="section">
		<h2>Hotkeys</h2>
		<div class="form-row">
			<label for="hotkey-binding">Key Binding</label>
			<HotkeyInput id="hotkey-binding" value={hotkeyBinding} onChange={(v) => { hotkeyBinding = v; }} />
		</div>
		<div class="form-row">
			<label for="hotkey-mode">Mode</label>
			<MaterialSelect id="hotkey-mode" value={hotkeyMode} options={[{ value: 'toggle', label: 'Toggle (press to start/stop)' }, { value: 'hold', label: 'Hold (push-to-talk)' }]} onChange={(v) => { hotkeyMode = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Audio</h2>
		<div class="form-row">
			<label for="audio-sample-rate">Sample Rate</label>
			<MaterialNumberField id="audio-sample-rate" value={audio.sample_rate} onChange={(v) => { audio.sample_rate = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-channels">Channels</label>
			<MaterialNumberField id="audio-channels" value={audio.channels} min={1} max={2} onChange={(v) => { audio.channels = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-max-duration">Max Duration (sec)</label>
			<MaterialNumberField id="audio-max-duration" value={audio.max_duration_secs} min={10} max={300} onChange={(v) => { audio.max_duration_secs = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-silence-timeout">Silence Timeout (ms)</label>
			<MaterialNumberField id="audio-silence-timeout" value={audio.silence_timeout_ms} min={500} max={10000} step={100} onChange={(v) => { audio.silence_timeout_ms = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-vad-threshold">VAD Threshold</label>
			<input id="audio-vad-threshold" type="range" class="md-slider" bind:value={audio.vad_threshold} min="0" max="1" step="0.05" style="--vad-fill: {audio.vad_threshold * 100}%" />
			<span class="range-value">{audio.vad_threshold}</span>
		</div>
	</div>

	<div class="section">
		<h2>STT (Speech-to-Text)</h2>
		<p class="model-hint">Provider 与全部配置（API Key / Model / Base URL / MCP Server）都在 Audio Model 卡的 Provider 下拉框及其字段中完成。此处仅设置转写超时。</p>
		<div class="form-row">
			<label for="stt-timeout">Timeout (sec)</label>
			<MaterialNumberField id="stt-timeout" value={stt.timeout_secs} min={5} max={600} onChange={(v) => { stt.timeout_secs = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Task</h2>
		<div class="form-row">
			<label for="task-max-concurrent">Max Concurrent</label>
			<MaterialNumberField id="task-max-concurrent" value={task.max_concurrent} min={1} max={10} onChange={(v) => { task.max_concurrent = v; }} />
		</div>
		<div class="form-row">
			<label for="task-max-steps">Max Steps</label>
			<MaterialNumberField id="task-max-steps" value={task.max_steps} min={1} max={100} onChange={(v) => { task.max_steps = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Memory</h2>
		<div class="form-row">
			<label for="memory-window-size">Window Size</label>
			<MaterialNumberField id="memory-window-size" value={memory.session_window_size} min={10} max={500} onChange={(v) => { memory.session_window_size = v; }} />
		</div>
		<div class="form-row">
			<label for="memory-retention">Retention (days)</label>
			<MaterialNumberField id="memory-retention" value={memory.history_retention_days} min={1} max={365} onChange={(v) => { memory.history_retention_days = v; }} />
		</div>
	</div>

	<div class="section appearance-section">
		<h2>Appearance</h2>
		<div class="form-row">
			<span class="form-label">Theme</span>
			<div class="theme-toggle-row" role="radiogroup" aria-label="Theme">
				<button
					class="md-btn"
					class:md-btn--outlined={currentTheme === 'light'}
					class:md-btn--filled={currentTheme !== 'light'}
					role="radio"
					aria-checked={currentTheme === 'light'}
					onclick={() => themeStore.setTheme('light')}
				>Light</button>
				<button
					class="md-btn"
					class:md-btn--outlined={currentTheme === 'dark'}
					class:md-btn--filled={currentTheme !== 'dark'}
					role="radio"
					aria-checked={currentTheme === 'dark'}
					onclick={() => themeStore.setTheme('dark')}
				>Dark</button>
			</div>
		</div>
		<div class="form-row">
			<span class="form-label">Accent Color</span>
			<div class="accent-picker" role="radiogroup" aria-label="Accent color">
				{#each Object.entries(themeStore.presets) as [key, preset]}
					<button
						class="md-btn"
						class:accent-swatch-selected={accent === key}
						style="background: {preset.hex}; color: {contrastText(preset.hex)}; --_btn-state: {contrastText(preset.hex)}; border: 2px solid transparent; border-color: {accent === key ? contrastText(preset.hex) : 'transparent'}"
						role="radio"
						aria-checked={accent === key}
						aria-label="{preset.label} {preset.hex}"
						onclick={() => { accent = key; themeStore.setAccent(key); }}
					>{preset.label}</button>
				{/each}
				<button
					class="md-btn md-btn--filled"
					class:md-btn--outlined={accent.startsWith('#') || accent.startsWith('custom:')}
					role="radio"
					aria-checked={accent.startsWith('#') || accent.startsWith('custom:')}
					aria-label="Custom hex color"
				>
					<input
						id="custom-accent"
						type="text"
						class="custom-hex-input"
						placeholder="#RRGGBB"
						maxlength="7"
						value={customAccentHex}
						autocomplete="off"
						oninput={(e) => {
							const val = /** @type {HTMLInputElement} */(e.target).value;
							customAccentHex = val;
							if (/^#[0-9a-f]{6}$/i.test(val)) {
								accent = val;
								themeStore.setAccent(val);
							}
						}}
					/>
				</button>
			</div>
		</div>
	</div>

	<div class="section pref-section">
		<h2>Preferences</h2>
		<p class="model-hint" style="margin-bottom: var(--md-sys-space-md)">Learned preferences from your interactions. User-set values take priority over inferred values.</p>
		{#if prefLoaded && preferences.length > 0}
			<div class="pref-list">
				{#each preferences as [key, value]}
					<div class="pref-row">
						<span class="pref-key">{key.trimStart('inferred.')}</span>
						<span class="pref-value">
							{#if value.startsWith('[inferred]')}
								<span class="pref-tag pref-tag--inf">inferred</span>
							{:else if value.startsWith('[user]')}
								<span class="pref-tag pref-tag--user">user</span>
							{/if}
							{value.replace('[inferred] ', '').replace('[user] ', '')}
						</span>
						<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => deletePrefKey(key)} title="Delete preference">
							&times;
						</button>
					</div>
				{/each}
			</div>
		{:else if prefLoaded}
			<p class="model-hint">No preferences recorded yet. They will appear here as you use Haven.</p>
		{/if}
	</div>

	<div class="section">
		<h2>Security</h2>
		<div class="form-row">
			<label for="security-min-level">Minimum Confirmation Level</label>
			<MaterialSelect id="security-min-level" value={security.min_risk_level} options={[
				{ value: 'safe', label: 'None (all auto-approved)' },
				{ value: 'low', label: 'Low & above' },
				{ value: 'medium', label: 'Medium & above' },
				{ value: 'high', label: 'High & above' },
				{ value: 'critical', label: 'Critical only' },
			]} onChange={(v) => { security.min_risk_level = v; }} />
		</div>
		<p class="model-hint">Operations at or above this risk level will require your confirmation. Low-level operations (file read, window list) will auto-approve.</p>
	</div>

	<div class="section notification-section">
		<h2>Notifications</h2>
		<div class="notify-grid-header">
			<span class="switch-label"></span>
			<span class="switch-label">In-App Toast</span>
			<span class="switch-label">Windows</span>
		</div>
		{#each [
			{ key: 'task_created', label: 'Task Start' },
			{ key: 'task_completed', label: 'Task Complete' },
			{ key: 'task_paused', label: 'Task Paused' },
			{ key: 'task_error', label: 'Task Error' },
		] as ev (ev.key)}
			<div class="notify-grid-row">
				<span class="switch-label">{ev.label}</span>
				<MaterialSwitch checked={notification[ev.key].in_app} onChange={(v) => { notification[ev.key].in_app = v; }} />
				<MaterialSwitch checked={notification[ev.key].windows} onChange={(v) => { notification[ev.key].windows = v; }} />
			</div>
		{/each}
	</div>

	<div class="section log-section">
		<h2>Logging</h2>
		<div class="form-row switch-row">
			<span class="switch-label">File Logging</span>
			<MaterialSwitch checked={log.file_enabled} onChange={(v) => { log.file_enabled = v; }} />
		</div>
		<div class="form-row">
			<label for="log-level">Log Level</label>
			<MaterialSelect id="log-level" value={log.level} options={[
				{ value: 'trace', label: 'Trace' },
				{ value: 'debug', label: 'Debug' },
				{ value: 'info', label: 'Info' },
				{ value: 'warn', label: 'Warn' },
				{ value: 'error', label: 'Error' },
			]} onChange={(v) => { log.level = v; }} />
		</div>
	</div>

	<div class="section autostart-section">
		<h2>Autostart</h2>
		<div class="form-row autostart-row">
			<span class="autostart-label">Launch Haven on system startup</span>
				<MaterialSwitch checked={autostartEnabled} onChange={toggleAutostart} />
		</div>
	</div>

	<button class="md-btn md-btn--filled save-btn" onclick={saveSettings}>
		Save Settings
	</button>
</div>

<MaterialDialog
	open={keyChangeDialog.open}
	onClose={() => { keyChangeDialog = { open: false, model: '', label: '' }; newKeyValue = ''; }}
	title={keyChangeDialog.model ? (keyConfigured[keyChangeDialog.model] ? 'Change API Key' : 'Set API Key') : 'API Key'}
>
	{#snippet children()}
		<p class="dialog-hint">Enter the API key for <strong>{keyChangeDialog.label}</strong>.</p>
		<input
			type="password"
			class="md-input"
			bind:value={newKeyValue}
			placeholder="sk-..."
			autocomplete="new-password"
		/>
	{/snippet}
	{#snippet footer()}
		<button
			class="md-btn md-btn--text"
			onclick={() => { keyChangeDialog = { open: false, model: '', label: '' }; newKeyValue = ''; }}
		>
			Cancel
		</button>
		<button class="md-btn md-btn--filled" onclick={confirmKeyChange}>Confirm</button>
	{/snippet}
</MaterialDialog>

<style>
	.settings-page { max-width: var(--md-sys-content-max-width); }
	h1 { font-size: 24px; font-weight: 600; margin-bottom: var(--md-sys-space-xl); color: var(--md-sys-color-on-surface); }
	.section {
		background: var(--md-sys-color-surface-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-lg); margin-bottom: var(--md-sys-space-lg);
	}
	.section h2 {
		font-size: 13px; font-weight: 600; color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase; letter-spacing: 1px; margin-bottom: var(--md-sys-space-lg);
	}
	.model-card {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium); padding: var(--md-sys-space-md); margin-bottom: var(--md-sys-space-md);
	}
	.model-card h3 { font-size: 14px; font-weight: 600; color: var(--md-sys-color-primary); margin-bottom: var(--md-sys-space-md); }
	.model-group-heading {
		font-size: 13px; font-weight: 600; color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-lg); margin-bottom: var(--md-sys-space-sm);
		text-transform: uppercase; letter-spacing: 0.5px;
	}
	.model-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(340px, 1fr));
		gap: var(--md-sys-space-md);
	}
	.model-grid .model-card { margin-bottom: 0; }
	.model-input-row { display: flex; align-items: center; gap: var(--md-sys-space-sm); flex: 1; min-width: 0; }
	.model-input-row .md-input { flex: 1; min-width: 0; }
	.routing-heading {
		font-size: 13px; font-weight: 600; color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-lg); margin-bottom: var(--md-sys-space-sm);
		text-transform: uppercase; letter-spacing: 0.5px;
	}
	.model-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-top: calc(-1 * var(--md-sys-space-sm)); margin-bottom: var(--md-sys-space-md); }
	.form-row {
		display: flex; align-items: center; margin-bottom: var(--md-sys-space-sm); gap: var(--md-sys-space-md);
	}
	.form-row label,
	.form-row .form-label { width: 120px; color: var(--md-sys-color-on-surface-variant); font-size: 13px; flex-shrink: 0; }

	.cost-row {
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.cost-row label { width: auto; font-size: 12px; }
	.cost-row :global(.md-number-field) { width: 90px; flex-shrink: 0; }
	.cost-sep {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}
	.cost-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: calc(-1 * var(--md-sys-space-xs));
		margin-bottom: var(--md-sys-space-md);
	}

	.switch-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.switch-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}

	.notify-grid-header, .notify-grid-row {
		display: grid;
		grid-template-columns: 1fr auto auto;
		gap: var(--md-sys-space-md);
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
	}
	.notify-grid-header {
		padding-bottom: var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		margin-bottom: var(--md-sys-space-md);
	}
	.notify-grid-header .switch-label {
		font-weight: 600;
		font-size: 11px;
		text-transform: uppercase;
	}

	.form-row input[type='range'] { flex: 1; }
	.md-slider {
		-webkit-appearance: none;
		appearance: none;
		width: 100%;
		height: 4px;
		outline: none;
		cursor: pointer;
		flex: 1;
		margin: 18px 0;
		padding: 0;
		background: transparent;
	}
	.md-slider::-webkit-slider-runnable-track {
		height: 4px;
		border-radius: 2px;
		background: linear-gradient(to right, var(--md-sys-color-primary) var(--vad-fill, 50%), var(--md-sys-color-surface-container-highest) var(--vad-fill, 50%));
	}
	.md-slider::-webkit-slider-thumb {
		-webkit-appearance: none;
		appearance: none;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--md-sys-color-primary);
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-1);
		margin-top: -6px;
		transition: transform var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.md-slider::-webkit-slider-thumb:hover {
		transform: scale(1.25);
	}
	.md-slider::-moz-range-track {
		height: 4px;
		border-radius: 2px;
		background: var(--md-sys-color-surface-container-highest);
		border: none;
	}
	.md-slider::-moz-range-thumb {
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--md-sys-color-primary);
		border: none;
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-1);
	}
	.md-slider::-moz-range-thumb:hover {
		transform: scale(1.25);
	}
	.md-slider:focus-visible::-webkit-slider-thumb {
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--md-sys-color-primary) 20%, transparent);
	}
	.md-slider:focus-visible::-moz-range-thumb {
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--md-sys-color-primary) 20%, transparent);
	}
	.range-value {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 40px;
		min-width: 44px;
		padding: 0 var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		font-weight: 500;
	}
	.autostart-section .autostart-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.autostart-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}
	.save-btn {
		position: sticky;
		bottom: var(--md-sys-space-xs);
		display: block;
		margin: var(--md-sys-space-md) auto 0;
		z-index: 1;
	}
	.key-status-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 1;
		min-height: var(--md-comp-textfield-container-height);
	}
	.key-configured-label {
		color: var(--md-sys-color-success);
		font-size: 13px;
		font-weight: 500;
		flex: 1;
	}
	.key-status-row.key-not-configured {
		color: var(--md-sys-color-on-surface-variant);
	}
	.key-status-row.key-not-configured .key-configured-label {
		color: var(--md-sys-color-on-surface-variant);
	}
	.dialog-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		line-height: 1.5;
		margin-bottom: var(--md-sys-space-lg);
	}
	.pref-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.pref-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) 0;
	}
	.pref-key {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
		font-weight: 500;
		min-width: 140px;
		flex-shrink: 0;
	}
	.pref-value {
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		flex: 1;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.pref-tag {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 20px;
		padding: 0 6px;
		border-radius: var(--md-sys-shape-small);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		flex-shrink: 0;
	}
	.pref-tag--user {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.pref-tag--inf {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.pref-section .model-hint {
		font-size: 12px;
	}
	.theme-toggle-row {
		display: flex;
		gap: var(--md-sys-space-sm);
		flex: 1;
	}
	.accent-picker {
		display: flex;
		gap: var(--md-sys-space-sm);
		flex: 1;
		flex-wrap: wrap;
	}
	.accent-swatch-selected {
		outline: 2px solid var(--md-sys-color-on-surface);
		outline-offset: -2px;
	}
	.custom-hex-input {
		width: 84px;
		font-family: var(--md-sys-typescale-mono);
		font-size: 14px;
		background: transparent;
		border: none;
		outline: none;
		color: inherit;
		padding: 0;
		text-align: center;
	}
	.custom-hex-input::placeholder {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.6;
	}
</style>
