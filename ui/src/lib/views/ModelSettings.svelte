<script>
	import logger from '$lib/logger.ts';
	import { invoke } from '$lib/tauri.ts';
	import { addNotification } from '$lib/stores.ts';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import StatusDot from '$lib/StatusDot.svelte';
	import ApiKeyDialog from '$lib/ApiKeyDialog.svelte';
	import { ROLE_KEYS, modelCards } from '$lib/modelRoles.ts';

	/**
	 * LLM model configuration (providers + role slots).
	 *
	 * - The model library is gone as a manual list: each configured provider's
	 *   `/models` endpoint is fetched (auto-refreshed in the background on
	 *   load + a manual refresh button) and cached per provider; roles pick a
	 *   provider + one model id from that fetched list.
	 * - `llmConfig` is the shared, mutable state passed down from the settings
	 *   page: `{ providers: [], roles: [], stt_use_audio_model,
	 *   vision_use_image_model, max_concurrent_requests }`.
	 *
	 * @prop {object} llmConfig — shared LlmConfig state (mutable)
	 * @prop {object} stt — shared media.stt state (mutable; audio card binds to it)
	 * @prop {object} contextLimits — shared context_limits state (mutable)
	 * @prop {object} keyConfigured — {role|mediaKey: bool} key status (mutable)
	 * @prop {object} keyConfiguredProviders — per-provider key status
	 * @prop {string[]} mcpServerNames — configured MCP server names
	 * @prop {boolean} loaded — true once the parent finished loading settings
	 */
	let {
		llmConfig,
		stt,
		contextLimits,
		keyConfigured,
		keyConfiguredProviders = {},
		mcpServerNames = [],
		loaded = false,
	} = $props();

	// ---------------------------------------------------------------------
	// Role ↔ config helpers
	// ---------------------------------------------------------------------

	const roleCards = modelCards;

	/**
	 * @param {string} key
	 */
	function roleFor(key) {
		return (llmConfig.roles || []).find((/** @type {any} */ r) => r.role === key) || null;
	}

	/** Insert a default slot for a role (returns it), keeping the shared state
	 *  a plain array the settings page's save flow can serialise. */
	/**
	 * @param {string} key
	 */
	function ensureRole(key) {
		const existing = roleFor(key);
		if (existing) return existing;
		const slot = {
			role: key,
			provider: '',
			model: '',
			temperature: null,
			context_window: null,
			cost_per_1k_input_tokens: null,
			cost_per_1k_output_tokens: null,
		};
		llmConfig.roles.push(slot);
		return slot;
	}

	/**
	 * @param {string} name
	 */
	function providerByName(name) {
		return (llmConfig.providers || []).find((/** @type {any} */ p) => p.name === name);
	}

	function providerOptions() {
		return [{ value: '', label: '未配置' }, ...(llmConfig.providers || []).map((/** @type {any} */ p) => ({ value: p.name, label: p.name }))];
	}

	/**
	 * @param {string} key
	 * @param {string} providerName
	 */
	function setRoleProvider(key, providerName) {
		const slot = ensureRole(key);
		slot.provider = providerName;
		// A model from another provider almost never exists here: reset it and
		// force a fresh pick from the new provider's fetched list.
		slot.model = '';
		if (providerName) refreshProviderModels(providerName);
	}

	/**
	 * @param {string} providerName
	 */
	function roleModelOptions(providerName) {
		if (!providerName) return [];
		return (modelsByProvider[providerName] || []).map((/** @type {any} */ m) => ({ value: m.id, label: m.name || m.id }));
	}

	/**
	 * @param {string} providerName
	 */
	function roleModelLoading(providerName) {
		if (!providerName) return false;
		return !!modelFetching[providerName];
	}

	/**
	 * @param {any} p
	 */
	function isLocalProvider(p) {
		return p?.api_style === 'llama.cpp' || p?.provider === 'llama.cpp';
	}

	// ---------------------------------------------------------------------
	// Model list discovery (cached per provider, auto-refresh + manual)
	// ---------------------------------------------------------------------

	/** provider name → fetched [ModelInfo]. */
	/** @type {Record<string, any[]>} */
	let modelsByProvider = $state({});
	/** provider name → bool (in-flight fetch). */
	/** @type {Record<string, boolean>} */
	let modelFetching = $state({});
	/** One global "refresh" that refetches every configured provider. */
	let refreshingAll = $state(false);
	/** Timestamp of the last global refresh notice, to avoid spam. */
	let lastRefreshNotify = $state(0);

	/**
	 * Key status is authoritative from `get_api_key_status` (plus an unsaved
	 * key typed in the edit dialog). Do NOT infer from `modelsByProvider`:
	 * large `/models` responses delay that map and would flash 「未配置」.
	 * @param {any} p
	 */
	function isProviderKeyConfigured(p) {
		if (!p) return false;
		if (p.api_key) return true;
		if (keyConfiguredProviders[p.name]) return true;
		return isLocalProvider(p);
	}

	async function refreshAllModels(silent = false) {
		const providers = (llmConfig.providers || []).filter((/** @type {any} */ p) => p.base_url.trim());
		if (providers.length === 0) return;
		refreshingAll = true;
		try {
			if (providers.some((/** @type {any} */ p) => p.api_key)) {
				// Some provider has an unsaved key (typed in the dialog): fetch
				// each provider directly so a fresh key works before it is
				// persisted by the settings save.
				await Promise.allSettled(providers.map((/** @type {any} */ p) => refreshProviderModels(p.name)));
			} else {
				const map = await invoke('discover_all_models');
				modelsByProvider = map || {};
			}
			if (!silent) {
				const now = Date.now();
				if (now - lastRefreshNotify > 2500) {
					lastRefreshNotify = now;
					addNotification('模型列表已刷新', 'success', 2500);
				}
			}
		} catch (e) {
			const msg = typeof e === 'string' ? e : (e instanceof Error ? e.message : String(e));
			addNotification(`刷新模型列表失败: ${msg}`, 'error', 4000);
		} finally {
			refreshingAll = false;
		}
	}

	/** Refetch one provider's model list — with its unsaved key when present,
	 *  else the stored key (matched by base URL in the backend). */
	/**
	 * @param {string} providerName
	 */
	async function refreshProviderModels(providerName) {
		const p = providerByName(providerName);
		if (!p || !p.base_url.trim()) return;
		modelFetching[providerName] = true;
		try {
			const list = await invoke('discover_models', {
				baseUrl: p.base_url,
				apiKey: p.api_key || '',
				provider: providerName,
			});
			modelsByProvider = { ...modelsByProvider, [providerName]: list || [] };
		} catch (e) {
			const msg = typeof e === 'string' ? e : (e instanceof Error ? e.message : String(e));
			logger.warn('ModelSettings', `discover_models ${providerName} error`, msg);
			modelsByProvider = { ...modelsByProvider, [providerName]: [] };
		} finally {
			modelFetching[providerName] = false;
		}
	}

	let autoRefreshed = $state(false);
	// Non-blocking auto-refresh once settings are loaded and a first set of
	// providers (with keys) exists.
	$effect(() => {
		if (loaded && !autoRefreshed && (llmConfig.providers || []).length > 0) {
			autoRefreshed = true;
			refreshAllModels(true);
		}
	});

	// Materialize the six role slots in the shared state after load, so the
	// pickers always bind to a real slot (assignments only happen on user
	// actions, never during render).
	let rolesInitialized = $state(false);
	$effect(() => {
		if (loaded && !rolesInitialized) {
			rolesInitialized = true;
			for (const k of ROLE_KEYS) ensureRole(k);
		}
	});

	// ---------------------------------------------------------------------
	// STT sub-configuration (the audio role's transcription backend)
	// ---------------------------------------------------------------------

	const STT_PROVIDER_OPTIONS = [
		{ value: 'llm', label: '音频模型 (Audio Model)' },
		{ value: 'openai', label: 'OpenAI Whisper' },
		{ value: 'groq', label: 'Groq' },
		{ value: 'gemini', label: 'Google Gemini' },
		{ value: 'deepgram', label: 'Deepgram' },
		{ value: 'assemblyai', label: 'AssemblyAI' },
		{ value: 'mcp', label: 'MCP Server' },
		{ value: 'none', label: 'None' },
	];
	const OPENAI_COMPAT_STT = new Set(['openai', 'groq']);
	const GEMINI_STT = new Set(['gemini']);

	/**
	 * @param {string} provider
	 */
	function isOpenAiCompatibleStt(provider) {
		return OPENAI_COMPAT_STT.has(provider);
	}

	/**
	 * @param {string} provider
	 */
	function isGeminiStt(provider) {
		return GEMINI_STT.has(provider);
	}

	/**
	 * @param {string} provider
	 */
	function isCloudSttProvider(provider) {
		return ['openai', 'groq', 'gemini', 'deepgram', 'assemblyai'].includes(provider);
	}

	/**
	 * @param {string} provider
	 */
	function sttModelPlaceholder(provider) {
		if (provider === 'deepgram') return 'nova-3';
		if (provider === 'assemblyai') return 'assemblyai_default';
		if (provider === 'groq') return 'whisper-large-v3-turbo';
		if (isGeminiStt(provider)) return 'gemini-2.5-flash';
		return 'whisper-1';
	}

	/**
	 * @param {string} provider
	 */
	function sttBasePlaceholder(provider) {
		return isGeminiStt(provider) ? 'https://generativelanguage.googleapis.com/v1beta' : 'https://api.openai.com/v1';
	}

	/**
	 * @param {string} provider
	 */
	function sttFetchBaseUrl(provider) {
		if (stt.base_url.trim()) return stt.base_url.trim();
		if (provider === 'groq') return 'https://api.groq.com/openai/v1';
		if (provider === 'gemini') return 'https://generativelanguage.googleapis.com/v1beta';
		return 'https://api.openai.com/v1';
	}

	/** @type {any[]} */
	let sttModels = $state([]);
	let sttFetching = $state(false);
	/** @type {ReturnType<typeof setTimeout> | undefined} */
	let sttFetchTimer = undefined;

	/**
	 * @param {string} provider
	 */
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
		return (sttModels || []).map((m) => ({ value: m.id, label: m.name || m.id }));
	}

	async function fetchSttModels() {
		const provider = stt.provider;
		if (provider === 'llm' || provider === 'deepgram' || provider === 'assemblyai' || provider === 'mcp' || provider === 'none') {
			return;
		}
		const base = sttFetchBaseUrl(provider);
		if (!base || (!stt.api_key && !keyConfigured.stt)) {
			sttModels = [];
			return;
		}
		sttFetching = true;
		try {
			const list = await invoke('discover_models', { baseUrl: base, apiKey: stt.api_key, role: 'stt' });
			sttModels = list || [];
		} catch (e) {
			sttModels = [];
			const msg = typeof e === 'string' ? e : (e instanceof Error ? e.message : String(e));
			addNotification(`获取 STT 模型失败: ${msg}`, 'error', 4000);
		} finally {
			sttFetching = false;
		}
	}

	function scheduleSttFetch() {
		clearTimeout(sttFetchTimer);
		sttFetchTimer = setTimeout(() => fetchSttModels(), 500);
	}

	// ---------------------------------------------------------------------
	// Provider add / edit / delete
	// ---------------------------------------------------------------------

	/** @type {{ idx: number | null, form: { name: string, api_style: string, base_url: string, api_key: string } | null }} */
	let providerDialog = $state({ idx: null, form: null });

	function startAddProvider() {
		providerDialog = {
			idx: null,
			form: {
				name: '',
				api_style: 'openai-chat',
				base_url: 'https://api.openai.com/v1',
				api_key: '',
			},
		};
	}

	/**
	 * @param {number} idx
	 */
	function startEditProvider(idx) {
		const p = llmConfig.providers[idx];
		providerDialog = {
			idx,
			form: {
				name: p.name,
				api_style: p.api_style || 'openai-chat',
				base_url: p.base_url,
				api_key: '', // masked
			},
		};
	}

	function saveProvider() {
		if (!providerDialog?.form) return;
		const { idx, form } = providerDialog;
		const name = form.name.trim();
		if (!name) {
			addNotification('请填写 Provider 名称', 'error', 3000);
			return;
		}
		const others = llmConfig.providers.filter((/** @type {any} */ _, /** @type {number} */ i) => i !== idx);
		if (others.some((/** @type {any} */ p) => p.name === name)) {
			addNotification('Provider 名称已存在', 'error', 3000);
			return;
		}
		const prevKey = idx !== null ? llmConfig.providers[idx]?.api_key || '' : '';
		const preset = API_STYLE_PRESETS[form.api_style] || API_STYLE_PRESETS['openai-chat'];
		const provider = {
			name,
			provider: preset.provider,
			api_style: form.api_style,
			base_url: form.base_url.trim(),
			api_key: form.api_key || prevKey,
			auth_header_name: preset.auth_header_name,
			auth_header_prefix: preset.auth_header_prefix,
			proxy_url: null,
			no_proxy: null,
			default_max_tokens: null,
			default_temperature: null,
			default_timeout_secs: isSttOnlyStyle(form.api_style) ? 30 : null,
			default_timeout_streaming_secs: null,
			default_web_search: null,
		};
		if (idx === null) {
			llmConfig.providers.push(provider);
		} else {
			const oldName = llmConfig.providers[idx].name;
			llmConfig.providers[idx] = provider;
			if (oldName !== name) {
				// Keep role references pointing at the renamed provider.
				for (const r of llmConfig.roles) {
					if (r.provider === oldName) r.provider = name;
				}
				if (keyConfiguredProviders[oldName]) {
					delete keyConfiguredProviders[oldName];
					keyConfiguredProviders[name] = true;
				}
			}
		}
		if (provider.api_key || isLocalProvider(provider)) {
			keyConfiguredProviders[name] = true;
		}
		providerDialog = { idx: null, form: null };
		addNotification(provider.name ? `Provider 已保存` : 'Provider 已保存', 'success', 2000);
		refreshAllModels(true);
	}

	/**
	 * @param {number} idx
	 */
	function deleteProvider(idx) {
		const p = llmConfig.providers[idx];
		if (!p) return;
		// Detach every role that referenced the deleted provider.
		for (const r of llmConfig.roles) {
			if (r.provider === p.name) {
				r.provider = '';
				r.model = '';
			}
		}
		llmConfig.providers.splice(idx, 1);
		delete modelsByProvider[p.name];
		addNotification(`已删除 Provider ${p.name}`, 'success', 2000);
	}

	const API_STYLE_OPTIONS = [
		{ value: 'openai-chat', label: 'OpenAI Chat Completions' },
		{ value: 'llama.cpp', label: 'llama.cpp server (local)' },
		{ value: 'openai-responses', label: 'OpenAI Responses API' },
		{ value: 'anthropic', label: 'Anthropic (Claude)' },
		{ value: 'gemini', label: 'Google Gemini' },
		{ value: 'deepgram', label: 'Deepgram (STT only)' },
		{ value: 'assemblyai', label: 'AssemblyAI (STT only)' },
	];

	/** @type {Record<string, { base_url: string, provider: string, auth_header_name: string, auth_header_prefix: string }>} */
	const API_STYLE_PRESETS = {
		'openai-chat': {
			base_url: 'https://api.openai.com/v1',
			provider: 'openai',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
		},
		'llama.cpp': {
			base_url: 'http://127.0.0.1:8080/v1',
			provider: 'llama.cpp',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
		},
		'openai-responses': {
			base_url: 'https://api.openai.com/v1',
			provider: 'openai',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
		},
		anthropic: {
			base_url: 'https://api.anthropic.com',
			provider: 'anthropic',
			auth_header_name: 'x-api-key',
			auth_header_prefix: '',
		},
		gemini: {
			base_url: 'https://generativelanguage.googleapis.com/v1beta',
			provider: 'gemini',
			auth_header_name: 'x-goog-api-key',
			auth_header_prefix: '',
		},
		deepgram: {
			base_url: 'https://api.deepgram.com',
			provider: 'deepgram',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Token',
		},
		assemblyai: {
			base_url: 'https://api.assemblyai.com',
			provider: 'assemblyai',
			auth_header_name: 'authorization',
			auth_header_prefix: '',
		},
	};

	/**
	 * @param {string} style
	 */
	function isSttOnlyStyle(style) {
		return style === 'deepgram' || style === 'assemblyai';
	}

	/**
	 * @param {string} style
	 */
	function applyApiStylePreset(style) {
		if (!providerDialog?.form) return;
		const preset = API_STYLE_PRESETS[style];
		if (!preset) return;
		providerDialog.form.api_style = style;
		providerDialog.form.base_url = preset.base_url;
	}

	// ---------------------------------------------------------------------
	// STT / provider API-key dialog
	// ---------------------------------------------------------------------

	let keyDlg = $state({ open: false, model: '', label: '' });

	/**
	 * @param {string} model
	 * @param {string} label
	 */
	function openKeyDialog(model, label) {
		keyDlg = { open: true, model, label };
	}

	/**
	 * @param {string} value
	 */
	function confirmSttKey(value) {
		stt.api_key = value;
		keyConfigured.stt = true;
		keyDlg = { open: false, model: '', label: '' };
	}

	/**
	 * @param {string} style
	 */
	function apiStyleLabel(style) {
		return API_STYLE_OPTIONS.find((o) => o.value === style)?.label || style || '自动';
	}

	// Keep the STT provider in sync: switching the audio role's STT provider
	// is independent from the audio ROLE's LLM provider.
	/**
	 * @param {string} v
	 */
	function setSttProvider(v) {
		stt.provider = v;
		if (v === 'llm' || v === 'mcp') {
			// No cloud model list to fetch.
			sttModels = [];
		}
	}
</script>

<div class="section">
	<div class="llm-head">
		<h2>LLM Configuration</h2>
		<div class="llm-head-actions">
			<button class="md-btn md-btn--outlined" onclick={() => refreshAllModels()} disabled={refreshingAll}>
				{refreshingAll ? '刷新中…' : '刷新模型列表'}
			</button>
			<button class="md-btn md-btn--outlined" onclick={startAddProvider}>添加 Provider</button>
		</div>
	</div>
	<p class="model-hint">模型库改为按 Provider 自动获取：添加 Provider（地址 + API Key）后，其 <code>/models</code> 列表会被拉取并缓存；每个模型角色只需选择 Provider 与其中的一个模型。可选参数（温度 / 上下文 / 成本）留空则用 Provider 默认或内置目录自动解析。</p>

	{#if (llmConfig.providers || []).length === 0}
		<div class="providers-empty">
			<p class="model-hint">尚未配置任何 Provider。点击「添加 Provider」开始配置。</p>
		</div>
	{:else}
		<div class="providers-list">
			{#each llmConfig.providers as p, idx (p.name)}
			<div class="provider-card">
				<div class="provider-main">
					<span class="provider-name">{p.name}</span>
					<span class="provider-desc">
						{apiStyleLabel(p.api_style)} · {p.base_url}
					</span>
					<span class="lib-key">
						<StatusDot color={isProviderKeyConfigured(p) ? 'success' : 'outline'} />
						{isProviderKeyConfigured(p) ? '已配置' : '未配置'}
					</span>
					{#if modelsByProvider[p.name]?.length}
						<span class="provider-models">{modelsByProvider[p.name].length} 个模型</span>
					{/if}
				</div>
				<div class="provider-actions">
					<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => refreshProviderModels(p.name)} title="重新获取该 Provider 的模型列表">
						刷新
					</button>
					<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => startEditProvider(idx)}>编辑</button>
					<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => deleteProvider(idx)}>删除</button>
				</div>
			</div>
			{/each}
		</div>
	{/if}

	<div class="model-list">
		<div class="model-group">Core Models</div>
		{#each roleCards.filter((c) => c.group === 'core') as card}
			{@render rolePicker(card)}
		{/each}
		<div class="model-group">Specialized Models</div>
		{#each roleCards.filter((c) => c.group === 'specialized') as card}
			{@render rolePicker(card)}
		{/each}
	</div>

	<p class="cost-hint">上下文窗口留空时自动从内置模型目录解析；成本（USD/1K token）留空则默认 0（不显示成本）。</p>
</div>

{#snippet rolePicker(/** @type {any} */ card)}
	{@const slot = roleFor(card.key)}
	{#if slot}
	<div class="model-card">
		<div class="picker-card">
			<div class="model-field model-role">
				<span class="field-label">{card.label}</span>
				<div class="role-hint">{card.hint}</div>
			</div>
			<div class="model-field">
				<span class="field-label">Provider</span>
				<MaterialSelect
					id="{card.prefix}-provider"
					value={slot.provider}
					options={providerOptions()}
					onChange={(/** @type {string} */ v) => setRoleProvider(card.key, v)}
				/>
			</div>
			<div class="model-field">
				<span class="field-label">Model</span>
				{#if slot.provider}
					<MaterialAutocomplete
						id="{card.prefix}-model"
						value={slot.model}
						options={roleModelOptions(slot.provider)}
						placeholder={slot.model ? slot.model : '从获取的模型列表中选择或输入'}
						loading={roleModelLoading(slot.provider)}
						onChange={(/** @type {string} */ v) => { slot.model = v; }}
						onFocus={() => {
							if (!modelsByProvider[slot.provider]?.length) {
								refreshProviderModels(slot.provider);
							}
						}}
					/>
				{:else}
					<span class="provider-note">先选择 Provider</span>
				{/if}
			</div>
		</div>
		<div class="model-row overrides-row">
			<div class="model-field">
				<span class="field-label">Temp（可选）</span>
				<MaterialNumberField
					id="{card.prefix}-temp"
					value={slot.temperature ?? 0.7}
					step={0.1}
					min={0}
					max={2}
					onChange={(/** @type {number} */ v) => { slot.temperature = v; }}
				/>
			</div>
			<div class="model-field">
				<span class="field-label">Context（可选）</span>
				<MaterialNumberField
					id="{card.prefix}-context-window"
					value={slot.context_window ?? 0}
					step={1024}
					min={0}
					onChange={(/** @type {number} */ v) => { slot.context_window = v > 0 ? Math.round(v) : null; }}
				/>
			</div>
			<div class="model-field">
				<span class="field-label">Cost $/1K in（可选）</span>
				<MaterialNumberField
					id="{card.prefix}-cost-in"
					value={slot.cost_per_1k_input_tokens ?? 0}
					step={0.01}
					min={0}
					onChange={(/** @type {number} */ v) => { slot.cost_per_1k_input_tokens = v; }}
				/>
			</div>
			<div class="model-field">
				<span class="field-label">Cost $/1K out（可选）</span>
				<MaterialNumberField
					id="{card.prefix}-cost-out"
					value={slot.cost_per_1k_output_tokens ?? 0}
					step={0.01}
					min={0}
					onChange={(/** @type {number} */ v) => { slot.cost_per_1k_output_tokens = v; }}
				/>
			</div>
		</div>
		{#if card.key === 'audio_model'}
			<div class="audio-stt-block">
				<h4>语音转写（STT）</h4>
				<p class="model-hint">推荐：上方 Audio Model 选 Whisper / Gemini / Deepgram / AssemblyAI Provider，STT Provider 选「音频模型」。也可在此直接配置独立云端 STT（与 Provider 库并行，旧配置仍可用）。</p>
				<div class="stt-grid">
					<div class="model-field">
						<span class="field-label">STT Provider</span>
						<MaterialSelect
							id="au-stt-provider"
							value={stt.provider}
							options={STT_PROVIDER_OPTIONS}
							onChange={setSttProvider}
						/>
					</div>
					{#if stt.provider === 'mcp'}
						<div class="model-field">
							<span class="field-label">MCP Server</span>
							<MaterialAutocomplete
								id="au-stt-mcp"
								value={stt.mcp_server}
								options={mcpServerNames.map((n) => ({ value: n, label: n }))}
								placeholder="Pick a configured MCP server"
								loading={false}
								onChange={(/** @type {string} */ v) => { stt.mcp_server = v; }}
							/>
						</div>
					{:else if isCloudSttProvider(stt.provider)}
						<div class="model-field">
							<span class="field-label">Base URL</span>
							{#if isOpenAiCompatibleStt(stt.provider) || isGeminiStt(stt.provider)}
								<input id="au-stt-base-url" type="text" class="md-input" bind:value={stt.base_url} placeholder={sttBasePlaceholder(stt.provider)} autocomplete="off" />
							{:else}
								<span class="provider-note">由提供商默认</span>
							{/if}
						</div>
						<div class="model-field">
							<span class="field-label">Model</span>
							<MaterialAutocomplete
								id="au-stt-model"
								value={stt.model}
								options={sttModelOptions(stt.provider)}
								placeholder={sttModelPlaceholder(stt.provider)}
								loading={sttFetching}
								onChange={(/** @type {string} */ v) => { stt.model = v; }}
								onFocus={() => scheduleSttFetch()}
							/>
						</div>
						<div class="model-field">
							<span class="field-label">API Key</span>
							<div class="key-cell" class:key-not-configured={!keyConfigured.stt}>
								<StatusDot color={keyConfigured.stt ? 'success' : 'outline'} />
								<button
									id="au-stt-api-key"
									class="md-btn md-btn--xs md-btn--outlined"
									title={keyConfigured.stt ? 'Configured' : 'Not Configured'}
									onclick={() => openKeyDialog('stt', 'STT API Key')}
								>
									{keyConfigured.stt ? 'Change' : 'Set'}
								</button>
							</div>
						</div>
					{/if}
				</div>
			</div>
		{/if}
	</div>
	{/if}
{/snippet}

{#if providerDialog.form}
{@const pdForm = providerDialog.form}
<MaterialDialog open={true} title={providerDialog.idx === null ? '添加 Provider' : '编辑 Provider'} onClose={() => { providerDialog = { idx: null, form: null }; }}>
	{#snippet children()}
		<div class="lib-form">
			<div class="model-field">
				<span class="field-label">名称</span>
				<input type="text" class="md-input" bind:value={pdForm.name} placeholder="唯一名称，角色据此选择" autocomplete="off" />
			</div>
			<div class="model-field">
				<span class="field-label">API Style（接线协议）</span>
				<MaterialSelect
					id="prov-api-style"
					value={pdForm.api_style}
					options={API_STYLE_OPTIONS}
					onChange={(/** @type {string} */ v) => applyApiStylePreset(v)}
				/>
			</div>
			{#if isSttOnlyStyle(pdForm.api_style)}
				<p class="model-hint">该协议仅支持语音转写。请将其分配给 Audio Model，并把录音 STT Provider 设为「音频模型」。</p>
			{/if}
			<div class="model-field">
				<span class="field-label">Base URL</span>
				<input type="text" class="md-input" bind:value={pdForm.base_url} placeholder="https://api.openai.com/v1" autocomplete="off" />
			</div>
			<div class="model-field">
				<span class="field-label">API Key</span>
				<input
					type="password"
					class="md-input"
					bind:value={pdForm.api_key}
					placeholder={providerDialog.idx !== null ? '已配置，留空保持不变' : ''}
					autocomplete="off"
				/>
			</div>
		</div>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn" onclick={() => { providerDialog = { idx: null, form: null }; }}>取消</button>
		<button class="md-btn md-btn--filled" onclick={saveProvider}>保存</button>
	{/snippet}
</MaterialDialog>
{/if}

<div class="section input-format-section">
	<h2>输入格式</h2>
	<p class="model-hint">每种输入格式的处理方式与限制。保存后对聊天输入框生效，后端校验使用相同配置。</p>

	<div class="format-card">
		<h3>文本 Text</h3>
		<p class="model-hint">文字指令直接发送给 Default Model 处理。语音转写结果也以文本形式进入同一通道，无需额外配置。</p>
	</div>

	<div class="format-card">
		<h3>图片 Image</h3>
		<p class="model-hint">
			粘贴或选取的图片先压缩为 JPEG（最长边 ≤{contextLimits.max_attachment_image_dim_px}px、质量
			{Math.round(contextLimits.attachment_image_jpeg_quality * 100)}%），再交由视觉模型理解；关闭专用模型后改由
			Default Model 处理。
		</p>
		<div class="form-row switch-row">
			<span class="switch-label">图片理解使用专用视觉模型</span>
			<MaterialSwitch checked={llmConfig.vision_use_image_model} onChange={(/** @type {boolean} */ v) => { llmConfig.vision_use_image_model = v; }} />
		</div>
		<div class="form-row">
			<label for="max-attachment-images">单条消息最多图片数</label>
			<MaterialNumberField
				id="max-attachment-images"
				value={contextLimits.max_attachment_images}
				min={1}
				max={20}
				step={1}
				onChange={(/** @type {number} */ v) => { contextLimits.max_attachment_images = v; }}
			/>
		</div>
		<div class="form-row">
			<label for="max-attachment-image-mb">单张图片大小上限 (MiB)</label>
			<MaterialNumberField
				id="max-attachment-image-mb"
				value={Math.round((contextLimits.max_attachment_image_bytes / 1048576) * 10) / 10}
				min={1}
				max={50}
				step={1}
				onChange={(/** @type {number} */ v) => { contextLimits.max_attachment_image_bytes = Math.round(v * 1024 * 1024); }}
			/>
		</div>
		<div class="form-row">
			<label for="max-attachment-image-dim">压缩最长边 (px)</label>
			<MaterialNumberField
				id="max-attachment-image-dim"
				value={contextLimits.max_attachment_image_dim_px}
				min={512}
				max={4096}
				step={64}
				onChange={(/** @type {number} */ v) => { contextLimits.max_attachment_image_dim_px = v; }}
			/>
		</div>
		<div class="form-row">
			<label for="attachment-image-quality">JPEG 压缩质量</label>
			<MaterialNumberField
				id="attachment-image-quality"
				value={contextLimits.attachment_image_jpeg_quality}
				min={0.1}
				max={1}
				step={0.05}
				onChange={(/** @type {number} */ v) => { contextLimits.attachment_image_jpeg_quality = v; }}
			/>
		</div>
	</div>

	<div class="format-card">
		<h3>文件 File</h3>
		<p class="model-hint">
			附件以 base64 上传，后端保存到磁盘，agent 通过 file 工具读取路径进行处理，无需额外配置。
		</p>
		<div class="form-row">
			<label for="max-attachment-files">单条消息最多文件数</label>
			<MaterialNumberField
				id="max-attachment-files"
				value={contextLimits.max_attachment_files}
				min={1}
				max={20}
				step={1}
				onChange={(/** @type {number} */ v) => { contextLimits.max_attachment_files = v; }}
			/>
		</div>
		<div class="form-row">
			<label for="max-attachment-file-mb">单个文件大小上限 (MiB)</label>
			<MaterialNumberField
				id="max-attachment-file-mb"
				value={Math.round((contextLimits.max_attachment_file_bytes / 1048576) * 10) / 10}
				min={1}
				max={100}
				step={1}
				onChange={(/** @type {number} */ v) => { contextLimits.max_attachment_file_bytes = Math.round(v * 1024 * 1024); }}
			/>
		</div>
	</div>

	<div class="format-card">
		<h3>语音 Voice</h3>
		<p class="model-hint">
			按住热键录音，经 STT 转写为文本后作为普通消息发送；转写可走专用音频模型或使用 Default Model。
		</p>
		<div class="form-row switch-row">
			<span class="switch-label">录音转写使用专用音频模型</span>
			<MaterialSwitch checked={llmConfig.stt_use_audio_model} onChange={(/** @type {boolean} */ v) => { llmConfig.stt_use_audio_model = v; }} />
		</div>
		<p class="model-hint">STT 提供商与录音参数（VAD、采样率、时长上限）在「常规 → Audio / STT」与上方 Audio 角色卡片中配置。</p>
	</div>
</div>

<ApiKeyDialog
	open={keyDlg.open}
	label={keyDlg.label}
	configured={keyDlg.model ? !!keyConfigured[keyDlg.model] : false}
	onClose={() => { keyDlg = { open: false, model: '', label: '' }; }}
	onConfirm={confirmSttKey}
/>

<style>
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
	.input-format-section {
		max-width: 640px;
	}
	.input-format-section .form-row :global(.md-number-field) {
		width: 200px;
	}
	.llm-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
	}
	.llm-head h2 { margin: 0; }
	.llm-head-actions {
		display: flex;
		gap: var(--md-sys-space-sm);
		flex-shrink: 0;
	}
	.model-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
		margin-top: var(--md-sys-space-lg);
	}
	.model-group {
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 1px;
		color: var(--md-sys-color-primary);
		margin: var(--md-sys-space-md) 0 var(--md-sys-space-xs);
	}
	.model-group:first-child { margin-top: 0; }
	.providers-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-md);
	}
	.providers-empty {
		margin-top: var(--md-sys-space-md);
	}
	.provider-card {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
	}
	.provider-main {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}
	.provider-name {
		font-size: 13px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.provider-desc {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.provider-models {
		font-size: 11px;
		color: var(--md-sys-color-primary);
	}
	.lib-key {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.provider-actions {
		display: flex;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
	}
	.lib-form {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.model-card {
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
		padding: var(--md-sys-space-md);
	}
	.picker-card {
		display: grid;
		grid-template-columns: 1.2fr 1fr 1.4fr;
		gap: var(--md-sys-space-lg);
		align-items: end;
	}
	.model-row {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
		gap: var(--md-sys-space-md);
		align-items: end;
	}
	.overrides-row {
		margin-top: var(--md-sys-space-md);
		padding-top: var(--md-sys-space-md);
		border-top: 1px dashed var(--md-sys-color-outline-variant);
	}
	.model-field {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.model-field .md-input { width: 100%; }
	.model-field :global(.md-number-field) { width: 100%; }
	.model-field :global(.md-select-container),
	.model-field :global(.ma-root) { width: 100%; }
	.field-label {
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
	}
	.model-role .field-label {
		color: var(--md-sys-color-primary);
		font-size: 13px;
	}
	.role-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-top: 2px; line-height: 1.4; }
	.provider-note { font-size: 11px; color: var(--md-sys-color-on-surface-variant); font-style: italic; }
	.key-cell {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-height: var(--md-comp-textfield-container-height);
	}
	.key-cell .md-btn { flex-shrink: 0; min-width: 64px; }
	.model-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-top: calc(-1 * var(--md-sys-space-sm)); margin-bottom: var(--md-sys-space-md); }
	.model-hint code {
		background: var(--md-sys-color-surface-container-highest);
		padding: 1px 4px;
		border-radius: 4px;
	}
	.form-row {
		display: flex; align-items: center; margin-bottom: var(--md-sys-space-sm); gap: var(--md-sys-space-md);
	}
	.form-row label { width: 120px; color: var(--md-sys-color-on-surface-variant); font-size: 13px; flex-shrink: 0; }
	.cost-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-md);
		margin-bottom: 0;
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
	.format-card {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-md);
	}
	.format-card h3 { font-size: 14px; font-weight: 600; color: var(--md-sys-color-primary); margin-bottom: var(--md-sys-space-sm); }
	.format-card .model-hint { margin-top: 0; margin-bottom: var(--md-sys-space-md); }
	.audio-stt-block {
		margin-top: var(--md-sys-space-md);
		padding-top: var(--md-sys-space-md);
		border-top: 1px dashed var(--md-sys-color-outline-variant);
	}
	.audio-stt-block h4 {
		font-size: 12px;
		font-weight: 600;
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-xs);
	}
	.stt-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
		gap: var(--md-sys-space-md);
		align-items: end;
	}
	@media (max-width: 700px) {
		.picker-card {
			grid-template-columns: 1fr;
			gap: var(--md-sys-space-md);
		}
	}
</style>