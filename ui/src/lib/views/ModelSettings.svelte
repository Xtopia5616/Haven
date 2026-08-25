<script>
	import logger from '$lib/logger.ts';
	import { invoke } from '$lib/tauri.ts';
	import { addNotification } from '$lib/stores.ts';
	import { formatError } from '$lib/formatError.ts';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import ApiKeyDialog from '$lib/ApiKeyDialog.svelte';
	import ApiKeyField from '$lib/ApiKeyField.svelte';
	import { emptyRoleSlot, ensureRoleSlots, modelCards } from '$lib/modelRoles.ts';
	import { inputFormats } from '$lib/inputFormats.ts';
	import {
		API_STYLE_OPTIONS,
		apiStylePreset,
		displayApiStyle,
		isKeylessProvider,
		isSttOnlyStyle,
		isTtsOnlyStyle,
		mediaCapabilityBackend,
		sttCapabilityBackend,
	} from '$lib/apiStyle.ts';

	/**
	 * Settings pages for LLM models and media channels.
	 *
	 * - `section="models"`: providers + role slots (model library via `/models`).
	 * - `section="media"`: voice / image / file / text channels (STT, OCR, TTS,
	 *   image gen, attachment limits).
	 * - `llmConfig` is shared mutable state from the settings page:
	 *   `{ providers, roles, stt_use_audio_model, vision_use_image_model,
	 *   max_concurrent_requests }`.
	 *
	 * @prop {'models' | 'media'} section
	 * @prop {object} llmConfig — shared LlmConfig state (mutable)
	 * @prop {object} audio — shared media.audio capture state (mutable; voice card)
	 * @prop {object} stt — shared media.stt state (mutable; voice card)
	 * @prop {object} ocr — shared media.ocr state (mutable; image card)
	 * @prop {object} tts — shared media.tts state (mutable; rewritten on provider rename/delete)
	 * @prop {object} imageGen — shared media.image_gen state (mutable; rewritten on provider rename/delete)
	 * @prop {object} contextLimits — shared context_limits state (mutable)
	 * @prop {object} keyConfigured — {role|mediaKey: bool} key status (mutable)
	 * @prop {object} keyConfiguredProviders — per-provider key status
	 * @prop {string[]} mcpServerNames — configured MCP server names
	 * @prop {boolean} loaded — true once the parent finished loading settings
	 * @prop {(fills: object[]) => void} [onDiscoverySettled] — fields
	 *   discovery actually wrote (role + context/cost). Parent patches only
	 *   those keys into the dirty snapshot so user Context/cost edits stay dirty.
	 */
	let {
		section = 'models',
		llmConfig,
		audio,
		stt,
		ocr,
		tts,
		imageGen,
		contextLimits,
		keyConfigured,
		keyConfiguredProviders = {},
		mcpServerNames = [],
		loaded = false,
		onDiscoverySettled = () => {},
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
		const slot = emptyRoleSlot(key);
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
		slot.context_window = null;
		slot.cost_per_1k_input_tokens = null;
		slot.cost_per_1k_output_tokens = null;
		if (providerName) refreshProviderModels(providerName);
	}

	/**
	 * Apply `/models` metadata onto a role slot when the user picks a model.
	 * @param {any} slot
	 * @param {string} providerName
	 * @param {string} modelId
	 * @param {{ overwrite?: boolean }} [opts] (overwrite=true replaces existing
	 *   Context/cost; false only fills empty slots (used when refreshing lists
	 *   for roles that still have null Context after the builtin catalog was
	 *   removed).
	 */
	function applyDiscoveredModelMeta(slot, providerName, modelId, opts = {}) {
		const overwrite = !!opts.overwrite;
		const list = modelsByProvider[providerName] || [];
		const m = list.find((/** @type {any} */ x) => x.id === modelId);
		if (!m) {
			if (overwrite) {
				slot.context_window = null;
				slot.cost_per_1k_input_tokens = null;
				slot.cost_per_1k_output_tokens = null;
			}
			return;
		}
		/** @type {Record<string, unknown>} */
		const wrote = {};
		if (overwrite || slot.context_window == null || slot.context_window === 0) {
			const next = m.context_window > 0 ? m.context_window : null;
			if (slot.context_window !== next) {
				slot.context_window = next;
				wrote.context_window = next;
			}
		}
		if (overwrite || slot.cost_per_1k_input_tokens == null) {
			const next =
				typeof m.cost_per_1k_input_tokens === 'number' ? m.cost_per_1k_input_tokens : null;
			if (slot.cost_per_1k_input_tokens !== next) {
				slot.cost_per_1k_input_tokens = next;
				wrote.cost_per_1k_input_tokens = next;
			}
		}
		if (overwrite || slot.cost_per_1k_output_tokens == null) {
			const next =
				typeof m.cost_per_1k_output_tokens === 'number' ? m.cost_per_1k_output_tokens : null;
			if (slot.cost_per_1k_output_tokens !== next) {
				slot.cost_per_1k_output_tokens = next;
				wrote.cost_per_1k_output_tokens = next;
			}
		}
		return wrote;
	}

	/** Fill empty Context/cost on existing roles from the latest `/models` map. */
	function backfillRoleMetaFromDiscovery() {
		/** @type {object[]} */
		const fills = [];
		for (const slot of llmConfig.roles || []) {
			if (!slot?.provider || !slot?.model) continue;
			const wrote = applyDiscoveredModelMeta(slot, slot.provider, slot.model, {
				overwrite: false,
			});
			if (wrote && Object.keys(wrote).length) {
				fills.push({ role: slot.role, ...wrote });
			}
		}
		onDiscoverySettled?.(fills);
	}

	/**
	 * @param {string} key
	 * @param {string} modelId
	 */
	function setRoleModel(key, modelId) {
		const slot = ensureRole(key);
		slot.model = modelId;
		applyDiscoveredModelMeta(slot, slot.provider, modelId, { overwrite: true });
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
		return isKeylessProvider(p);
	}

	/**
	 * @param {any} p
	 */
	function providerDisplayStyle(p) {
		return displayApiStyle(p);
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
			backfillRoleMetaFromDiscovery();
			if (!silent) {
				const now = Date.now();
				if (now - lastRefreshNotify > 2500) {
					lastRefreshNotify = now;
					addNotification('模型列表已刷新', 'success', 2500);
				}
			}
		} catch (e) {
			const msg = formatError(e);
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
			backfillRoleMetaFromDiscovery();
		} catch (e) {
			const msg = formatError(e);
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
	// actions, never during render). Parent also materializes before the
	// dirty snapshot; this remains a safety net if roles arrive later.
	let rolesInitialized = $state(false);
	$effect(() => {
		if (loaded && !rolesInitialized) {
			rolesInitialized = true;
			ensureRoleSlots(llmConfig.roles || []);
		}
	});

	// ---------------------------------------------------------------------
	// STT / OCR (voice + image input cards)
	// ---------------------------------------------------------------------

	const STT_SPECIAL = new Set(['llm', 'mcp', 'none']);
	const OCR_PROVIDER_OPTIONS = [
		{ value: 'llm', label: '视觉模型 (Image Model)' },
		{ value: 'baidu', label: 'Baidu 通用文字识别' },
		{ value: 'azure', label: 'Azure AI Vision' },
		{ value: 'tencent', label: 'Tencent 通用印刷体' },
		{ value: 'none', label: '未配置（透传图片）' },
	];

	/** TTS / 文生图：None + llm.providers 名称。 */
	function mediaProviderOptions(/** @type {string} */ current) {
		/** @type {{ value: string, label: string }[]} */
		const opts = [{ value: 'none', label: '未配置' }];
		for (const p of llmConfig.providers || []) {
			if (!p?.name) continue;
			opts.push({ value: p.name, label: p.name });
		}
		if (current && current !== 'none' && !opts.some((o) => o.value === current)) {
			opts.push({ value: current, label: `${current}（需重选「模型」页 Provider）` });
		}
		return opts;
	}

	/**
	 * @param {string} name
	 * @param {'tts' | 'image_gen'} capability
	 * @returns {'openai' | 'gemini' | 'elevenlabs' | ''}
	 */
	function mediaProviderKind(name, capability) {
		if (!name || name === 'none') return '';
		const p = providerByName(name);
		if (!p) {
			// Legacy capability ids when no matching llm.providers entry.
			if (name === 'openai' || name === 'elevenlabs' || name === 'gemini') {
				if (capability === 'tts' && name === 'gemini') return '';
				if (capability === 'image_gen' && name === 'elevenlabs') return '';
				return /** @type {'openai' | 'gemini' | 'elevenlabs'} */ (name);
			}
			return '';
		}
		return mediaCapabilityBackend(p, capability);
	}

	function sttProviderOptions() {
		/** @type {{ value: string, label: string }[]} */
		const opts = [
			{ value: 'llm', label: '音频模型 (Audio Model)' },
			{ value: 'mcp', label: 'MCP Server' },
			{ value: 'none', label: '未配置' },
		];
		for (const p of llmConfig.providers || []) {
			if (!p?.name) continue;
			opts.push({ value: p.name, label: p.name });
		}
		const current = stt.provider;
		if (current && !STT_SPECIAL.has(current) && !opts.some((o) => o.value === current)) {
			opts.push({ value: current, label: `${current}（需重选「模型」页 Provider）` });
		}
		return opts;
	}

	/**
	 * @param {string} name
	 * @returns {'openai' | 'groq' | 'gemini' | 'deepgram' | 'assemblyai' | ''}
	 */
	function sttBackendKind(name) {
		if (!name || STT_SPECIAL.has(name)) return '';
		if (['openai', 'groq', 'gemini', 'deepgram', 'assemblyai'].includes(name)) {
			return /** @type {'openai' | 'groq' | 'gemini' | 'deepgram' | 'assemblyai'} */ (name);
		}
		const p = providerByName(name);
		if (!p) return '';
		return sttCapabilityBackend(p);
	}

	function isNamedSttProvider(/** @type {string} */ provider) {
		return !!provider && !STT_SPECIAL.has(provider);
	}

	/**
	 * @param {string} kind
	 */
	function sttModelPlaceholder(kind) {
		if (kind === 'deepgram') return 'nova-3';
		if (kind === 'assemblyai') return 'assemblyai_default';
		if (kind === 'groq') return 'whisper-large-v3-turbo';
		if (kind === 'gemini') return 'gemini-2.5-flash';
		return 'whisper-1';
	}

	/** @type {any[]} */
	let sttModels = $state([]);
	let sttFetching = $state(false);
	/** @type {ReturnType<typeof setTimeout> | undefined} */
	let sttFetchTimer = undefined;

	/**
	 * @param {string} kind
	 */
	function sttModelOptions(kind) {
		if (kind === 'deepgram') {
			return [
				{ value: 'nova-3', label: 'nova-3' },
				{ value: 'nova-2', label: 'nova-2' },
				{ value: 'whisper-large-v3', label: 'whisper-large-v3' },
				{ value: 'whisper-large-v3-turbo', label: 'whisper-large-v3-turbo' },
			];
		}
		if (kind === 'assemblyai') {
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
		const name = stt.provider;
		if (!isNamedSttProvider(name)) return;
		const kind = sttBackendKind(name);
		if (!kind || kind === 'deepgram' || kind === 'assemblyai') return;
		const p = providerByName(name);
		const base = (p?.base_url || '').trim();
		const key = p?.api_key || '';
		const keyOk = !!key || !!keyConfiguredProviders[name] || !!keyConfigured.stt;
		if (!base || !keyOk) {
			sttModels = [];
			return;
		}
		sttFetching = true;
		try {
			const list = await invoke('discover_models', {
				baseUrl: base,
				apiKey: key,
				provider: name,
				role: 'stt',
			});
			sttModels = list || [];
		} catch (e) {
			sttModels = [];
			const msg = formatError(e);
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
				api_style: providerDisplayStyle(p),
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
		const prev = idx !== null ? llmConfig.providers[idx] : null;
		const prevKey = prev?.api_key || '';
		const preset = apiStylePreset(form.api_style);
		const provider = {
			...(prev || {}),
			name,
			provider: preset.provider,
			api_style: preset.api_style,
			base_url: form.base_url.trim(),
			api_key: form.api_key || prevKey,
			auth_header_name: preset.auth_header_name,
			auth_header_prefix: preset.auth_header_prefix,
			proxy_url: prev?.proxy_url ?? null,
			no_proxy: prev?.no_proxy ?? null,
			default_max_tokens: prev?.default_max_tokens ?? null,
			default_temperature: prev?.default_temperature ?? null,
			default_timeout_secs:
				prev?.default_timeout_secs ??
				(isSttOnlyStyle(preset.api_style) ? 30 : null),
			default_timeout_streaming_secs: prev?.default_timeout_streaming_secs ?? null,
			default_web_search: prev?.default_web_search ?? null,
		};
		if (idx === null) {
			llmConfig.providers.push(provider);
		} else {
			const oldName = llmConfig.providers[idx].name;
			llmConfig.providers[idx] = provider;
			if (oldName !== name) {
				// Keep role / media-capability references pointing at the renamed provider.
				for (const r of llmConfig.roles) {
					if (r.provider === oldName) r.provider = name;
				}
				if (stt?.provider === oldName) stt.provider = name;
				if (tts?.provider === oldName) tts.provider = name;
				if (imageGen?.provider === oldName) imageGen.provider = name;
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
		// Detach every role / media capability that referenced the deleted provider.
		for (const r of llmConfig.roles) {
			if (r.provider === p.name) {
				r.provider = '';
				r.model = '';
			}
		}
		if (stt?.provider === p.name) stt.provider = 'llm';
		if (tts?.provider === p.name) tts.provider = 'none';
		if (imageGen?.provider === p.name) imageGen.provider = 'none';
		llmConfig.providers.splice(idx, 1);
		delete modelsByProvider[p.name];
		addNotification(`已删除 Provider ${p.name}`, 'success', 2000);
	}

	/**
	 * @param {string} style
	 */
	function applyApiStylePreset(style) {
		if (!providerDialog?.form) return;
		const preset = apiStylePreset(style);
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
	function confirmMediaKey(value) {
		if (keyDlg.model === 'ocr') {
			ocr.api_key = value;
		} else if (keyDlg.model === 'ocr_secret') {
			ocr.api_secret = value;
		}
		keyConfigured[keyDlg.model] = true;
		keyDlg = { open: false, model: '', label: '' };
	}

	/**
	 * @param {string} style
	 */
	/**
	 * @param {any} pOrStyle
	 */
	function apiStyleLabel(pOrStyle) {
		const style =
			typeof pOrStyle === 'string' ? pOrStyle : providerDisplayStyle(pOrStyle);
		return API_STYLE_OPTIONS.find((o) => o.value === style)?.label || style || '自动';
	}

	// Keep the STT provider in sync: switching the audio role's STT provider
	// is independent from the audio ROLE's LLM provider.
	/**
	 * @param {string} v
	 */
	function setSttProvider(v) {
		stt.provider = v;
		if (!isNamedSttProvider(v)) {
			sttModels = [];
		}
	}
</script>

{#if section === 'media'}
<div class="section media-section">
	<h2>媒体</h2>
	<p class="model-hint">按模态配置输入与输出。STT / OCR 可走专用通道或「模型」页的 Audio / Image Model；TTS / 文生图复用「模型」页已添加的 Provider（Base URL + API Key）。</p>

	<div class="card-list">
		{#each inputFormats as format (format.id)}
			<div class="settings-card">
				<div class="card-head">
					<span class="card-title">{format.label}</span>
					<p class="card-hint">{format.hint}</p>
				</div>

				{#if format.id === 'voice'}
					<div class="capability-block first">
						<h4>输入 · 采集</h4>
						<div class="form-row switch-row">
							<span class="switch-label">录音转写使用专用音频模型</span>
							<MaterialSwitch checked={llmConfig.stt_use_audio_model} onChange={(/** @type {boolean} */ v) => { llmConfig.stt_use_audio_model = v; }} />
						</div>
						<div class="form-row">
							<label for="audio-sample-rate">Sample Rate</label>
							<MaterialNumberField id="audio-sample-rate" value={audio.sample_rate} onChange={(/** @type {number} */ v) => { audio.sample_rate = v; }} />
						</div>
						<div class="form-row">
							<label for="audio-channels">Channels</label>
							<MaterialNumberField id="audio-channels" value={audio.channels} min={1} max={2} onChange={(/** @type {number} */ v) => { audio.channels = v; }} />
						</div>
						<div class="form-row">
							<label for="audio-max-duration">Max Duration (sec)</label>
							<MaterialNumberField id="audio-max-duration" value={audio.max_duration_secs} min={10} max={300} onChange={(/** @type {number} */ v) => { audio.max_duration_secs = v; }} />
						</div>
						<div class="form-row">
							<label for="audio-silence-timeout">Silence Timeout (ms)</label>
							<MaterialNumberField id="audio-silence-timeout" value={audio.silence_timeout_ms} min={500} max={10000} step={100} onChange={(/** @type {number} */ v) => { audio.silence_timeout_ms = v; }} />
						</div>
						<div class="form-row">
							<label for="audio-vad-threshold">VAD Threshold</label>
							<input id="audio-vad-threshold" type="range" class="md-slider" value={audio.vad_threshold} min="0" max="1" step="0.05" style="--vad-fill: {audio.vad_threshold * 100}%" oninput={(/** @type {Event} */ e) => { audio.vad_threshold = Number(/** @type {HTMLInputElement} */ (e.currentTarget).value); }} />
							<span class="range-value">{audio.vad_threshold}</span>
						</div>
					</div>
					<div class="capability-block">
						<h4>输入 · 语音转写（STT）</h4>
						<p class="model-hint">推荐：「模型」页 Audio Model 选 Whisper / Gemini 等，此处 Provider 选「音频模型」。也可选已配置 Provider 或 MCP。</p>
						<div class="stt-grid">
							<div class="model-field">
								<span class="field-label">STT Provider</span>
								<MaterialSelect
									id="voice-stt-provider"
									value={stt.provider}
									options={sttProviderOptions()}
									onChange={setSttProvider}
								/>
							</div>
							{#if stt.provider === 'mcp'}
								<div class="model-field">
									<span class="field-label">MCP Server</span>
									<MaterialAutocomplete
										id="voice-stt-mcp"
										value={stt.mcp_server}
										options={mcpServerNames.map((n) => ({ value: n, label: n }))}
										placeholder="Pick a configured MCP server"
										loading={false}
										onChange={(/** @type {string} */ v) => { stt.mcp_server = v; }}
									/>
								</div>
							{:else if isNamedSttProvider(stt.provider)}
								{#if sttBackendKind(stt.provider)}
									<div class="model-field">
										<span class="field-label">Model</span>
										<MaterialAutocomplete
											id="voice-stt-model"
											value={stt.model}
											options={sttModelOptions(sttBackendKind(stt.provider))}
											placeholder={sttModelPlaceholder(sttBackendKind(stt.provider))}
											loading={sttFetching}
											onChange={(/** @type {string} */ v) => { stt.model = v; }}
											onFocus={() => scheduleSttFetch()}
										/>
									</div>
								{:else}
									<p class="model-hint">该 Provider 不支持 STT（需 OpenAI 兼容 / Gemini / Deepgram / AssemblyAI）。</p>
								{/if}
							{/if}
							{#if stt.provider !== 'none'}
								<div class="model-field">
									<span class="field-label">Timeout (sec)</span>
									<MaterialNumberField id="voice-stt-timeout" value={stt.timeout_secs} min={5} max={600} onChange={(/** @type {number} */ v) => { stt.timeout_secs = v; }} />
								</div>
								<div class="model-field">
									<span class="field-label">Min Confidence</span>
									<input id="voice-stt-min-confidence" type="range" class="md-slider" value={stt.min_confidence} min="0" max="1" step="0.05" style="--vad-fill: {stt.min_confidence * 100}%" oninput={(/** @type {Event} */ e) => { stt.min_confidence = Number(/** @type {HTMLInputElement} */ (e.currentTarget).value); }} />
									<span class="range-value">{stt.min_confidence}</span>
								</div>
							{/if}
						</div>
						<p class="model-hint">置信度低于阈值时回落主模型。仅 Deepgram / AssemblyAI / MCP 报告置信度；Whisper 等在失败或空结果时回落。</p>
					</div>
					<div class="capability-block">
						<h4>输出 · 语音合成（TTS）</h4>
						<p class="model-hint">「朗读这段话」「读出来」等请求合成语音并附到消息；选「模型」页已添加的 Provider。</p>
						<div class="stt-grid">
							<div class="model-field">
								<span class="field-label">Provider</span>
								<MaterialSelect id="tts-provider" value={tts.provider} options={mediaProviderOptions(tts.provider)} onChange={(/** @type {string} */ v) => { tts.provider = v; }} />
							</div>
							{#if tts.provider !== 'none'}
								{#if mediaProviderKind(tts.provider, 'tts') === 'elevenlabs'}
									<div class="model-field">
										<span class="field-label">Voice ID</span>
										<input id="tts-voice" type="text" class="md-input" bind:value={tts.voice} placeholder="elevenlabs voice id" autocomplete="off" />
									</div>
								{:else if mediaProviderKind(tts.provider, 'tts') === 'openai'}
									<div class="model-field">
										<span class="field-label">Model</span>
										<input id="tts-model" type="text" class="md-input" bind:value={tts.model} placeholder="tts-1 / gpt-4o-mini-tts" autocomplete="off" />
									</div>
									<div class="model-field">
										<span class="field-label">Voice</span>
										<input id="tts-voice" type="text" class="md-input" bind:value={tts.voice} placeholder="alloy / nova / echo…" autocomplete="off" />
									</div>
								{:else}
									<p class="model-hint">该 Provider 不支持 TTS（需 OpenAI 兼容）。</p>
								{/if}
								<div class="model-field">
									<span class="field-label">Timeout (sec)</span>
									<MaterialNumberField id="tts-timeout" value={tts.timeout_secs} min={5} max={300} onChange={(/** @type {number} */ v) => { tts.timeout_secs = v; }} />
								</div>
							{/if}
						</div>
					</div>
				{:else if format.id === 'image'}
					<div class="capability-block first">
						<h4>输入 · 附件与理解</h4>
						<p class="model-hint">
							当前压缩：最长边 ≤{contextLimits.max_attachment_image_dim_px}px、质量
							{Math.round(contextLimits.attachment_image_jpeg_quality * 100)}%。
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
					<div class="capability-block">
						<h4>输入 · 文字提取（OCR）</h4>
						<p class="model-hint">「提取文字」意图走 OCR；推荐选视觉模型。专用云 OCR 失败或低置信度时回落到 Image Model。</p>
						<div class="stt-grid">
							<div class="model-field">
								<span class="field-label">OCR Provider</span>
								<MaterialSelect
									id="img-ocr-provider"
									value={ocr.provider}
									options={OCR_PROVIDER_OPTIONS}
									onChange={(/** @type {string} */ v) => { ocr.provider = v; }}
								/>
							</div>
							{#if ocr.provider === 'baidu' || ocr.provider === 'tencent' || ocr.provider === 'azure'}
								<div class="model-field">
									<span class="field-label">API Key</span>
									<ApiKeyField
										id="img-ocr-api-key"
										configured={keyConfigured.ocr}
										onEdit={() => openKeyDialog('ocr', 'OCR API Key')}
									/>
								</div>
							{/if}
							{#if ocr.provider === 'baidu' || ocr.provider === 'tencent'}
								<div class="model-field">
									<span class="field-label">Secret Key</span>
									<ApiKeyField
										id="img-ocr-secret"
										configured={keyConfigured.ocr_secret}
										onEdit={() => openKeyDialog('ocr_secret', 'OCR Secret Key')}
									/>
								</div>
							{/if}
							{#if ocr.provider === 'azure'}
								<div class="model-field">
									<span class="field-label">Base URL</span>
									<input id="img-ocr-base-url" type="text" class="md-input" bind:value={ocr.base_url} placeholder="https://&lt;resource&gt;.cognitiveservices.azure.com" autocomplete="off" />
								</div>
							{/if}
							{#if ocr.provider !== 'none'}
								<div class="model-field">
									<span class="field-label">Timeout (sec)</span>
									<MaterialNumberField id="img-ocr-timeout" value={ocr.timeout_secs} min={5} max={300} onChange={(/** @type {number} */ v) => { ocr.timeout_secs = v; }} />
								</div>
								<div class="model-field">
									<span class="field-label">Min Confidence</span>
									<input id="img-ocr-min-confidence" type="range" class="md-slider" value={ocr.min_confidence} min="0" max="1" step="0.05" style="--vad-fill: {ocr.min_confidence * 100}%" oninput={(/** @type {Event} */ e) => { ocr.min_confidence = Number(/** @type {HTMLInputElement} */ (e.currentTarget).value); }} />
									<span class="range-value">{ocr.min_confidence}</span>
								</div>
							{/if}
						</div>
					</div>
					<div class="capability-block">
						<h4>输出 · 文生图</h4>
						<p class="model-hint">「画一只猫」「生成海报」等请求生成图片并附到消息；需 OpenAI 兼容或 Gemini Provider。</p>
						<div class="stt-grid">
							<div class="model-field">
								<span class="field-label">Provider</span>
								<MaterialSelect id="ig-provider" value={imageGen.provider} options={mediaProviderOptions(imageGen.provider)} onChange={(/** @type {string} */ v) => { imageGen.provider = v; }} />
							</div>
							{#if imageGen.provider !== 'none'}
								{#if mediaProviderKind(imageGen.provider, 'image_gen')}
									<div class="model-field">
										<span class="field-label">Model</span>
										<input id="ig-model" type="text" class="md-input" bind:value={imageGen.model} placeholder={mediaProviderKind(imageGen.provider, 'image_gen') === 'gemini' ? 'gemini-2.5-flash-image' : 'gpt-image-1'} autocomplete="off" />
									</div>
									<div class="model-field">
										<span class="field-label">Timeout (sec)</span>
										<MaterialNumberField id="ig-timeout" value={imageGen.timeout_secs} min={10} max={600} onChange={(/** @type {number} */ v) => { imageGen.timeout_secs = v; }} />
									</div>
								{:else}
									<p class="model-hint">该 Provider 不支持文生图（需 OpenAI 兼容或 Gemini）。</p>
								{/if}
							{/if}
						</div>
					</div>
				{:else if format.id === 'file'}
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
				{/if}
			</div>
		{/each}
	</div>
</div>
{/if}

{#if section === 'models'}
<div class="section">
	<div class="llm-head">
		<h2>模型配置</h2>
		<div class="llm-head-actions">
			<button class="md-btn md-btn--outlined" onclick={() => refreshAllModels()} disabled={refreshingAll}>
				{refreshingAll ? '刷新中…' : '刷新模型列表'}
			</button>
			<button class="md-btn md-btn--outlined" onclick={startAddProvider}>添加 Provider</button>
		</div>
	</div>
	<p class="model-hint">添加 Provider（地址 + API Key）后拉取 <code>/models</code>（含上下文长度、能力、定价等元数据）；选模型时自动填入 Context / 成本（若 Provider 有返回）。媒体能力在「媒体」页配置。</p>

	{#if (llmConfig.providers || []).length === 0}
		<div class="providers-empty">
			<p class="model-hint">尚未配置任何 Provider。点击「添加 Provider」开始配置。</p>
		</div>
	{:else}
		<div class="providers-list">
			{#each llmConfig.providers as p, idx (p.name)}
			<div class="provider-card">
				<div class="provider-main">
					<div class="provider-title">
						<span class="provider-name">{p.name}</span>
						<ApiKeyField mode="badge" configured={isProviderKeyConfigured(p)} />
					</div>
					<span class="provider-desc">
						{apiStyleLabel(p)} · {p.base_url}
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

	<div class="card-list model-list">
		<div class="model-group">Core Models</div>
		{#each roleCards.filter((c) => c.group === 'core') as card}
			{@render rolePicker(card)}
		{/each}
		<div class="model-group">Specialized Models</div>
		{#each roleCards.filter((c) => c.group === 'specialized') as card}
			{@render rolePicker(card)}
		{/each}
	</div>

	<p class="cost-hint">Context / 成本优先用 Provider 返回的元数据；均未填写时上下文回退到「限制」页的默认上下文窗口，成本按 0（不显示）。</p>
</div>
{/if}

{#snippet rolePicker(/** @type {any} */ card)}
	{@const slot = roleFor(card.key)}
	{#if slot}
	<div class="settings-card">
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
						onChange={(/** @type {string} */ v) => setRoleModel(card.key, v)}
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
				<span class="field-label">Context K（可选）</span>
				<MaterialNumberField
					id="{card.prefix}-context-window"
					value={slot.context_window != null && slot.context_window > 0
						? Math.round(slot.context_window / 1000)
						: 0}
					step={1}
					min={0}
					onChange={(/** @type {number} */ v) => {
						slot.context_window = v > 0 ? Math.round(v * 1000) : null;
					}}
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
				<span class="field-label">Provider 预设</span>
				<MaterialSelect
					id="prov-api-style"
					value={pdForm.api_style}
					options={API_STYLE_OPTIONS}
					onChange={(/** @type {string} */ v) => applyApiStylePreset(v)}
				/>
			</div>
			{#if apiStylePreset(pdForm.api_style)}
				{@const preset = apiStylePreset(pdForm.api_style)}
				<p class="model-hint">
					线协议 <code>{preset.api_style}</code>
					{#if preset.hint}
						— {preset.hint}
					{/if}
					{#if preset.docs_url || preset.console_url}
						{' '}
						{#if preset.docs_url}<a href={preset.docs_url} target="_blank" rel="noreferrer">文档</a>{/if}
						{#if preset.docs_url && preset.console_url} · {/if}
						{#if preset.console_url}<a href={preset.console_url} target="_blank" rel="noreferrer">控制台</a>{/if}
					{/if}
				</p>
			{/if}
			{#if isSttOnlyStyle(apiStylePreset(pdForm.api_style).api_style)}
				<p class="model-hint">该协议仅支持语音转写。请将其分配给 Audio Model，并在「媒体」页语音卡片把 STT Provider 设为「音频模型」。</p>
			{:else if isTtsOnlyStyle(apiStylePreset(pdForm.api_style).api_style)}
				<p class="model-hint">该协议仅支持语音合成。在「媒体」页语音卡片把 TTS Provider 设为此项，并填写 Voice ID。</p>
			{/if}
			<div class="model-field">
				<span class="field-label">Base URL</span>
				<input type="text" class="md-input" bind:value={pdForm.base_url} placeholder="https://api.openai.com/v1" autocomplete="off" />
			</div>
			<div class="model-field">
				<span class="field-label">API Key</span>
				<ApiKeyField
					mode="edit"
					bind:value={pdForm.api_key}
					configured={providerDialog.idx !== null && isProviderKeyConfigured(llmConfig.providers[providerDialog.idx])}
					placeholder={providerDialog.idx === null ? 'sk-...' : ''}
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

<ApiKeyDialog
	open={keyDlg.open}
	label={keyDlg.label}
	configured={keyDlg.model ? !!keyConfigured[keyDlg.model] : false}
	onClose={() => { keyDlg = { open: false, model: '', label: '' }; }}
	onConfirm={confirmMediaKey}
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
	.media-section {
		max-width: 640px;
	}
	.media-section .form-row :global(.md-number-field) {
		width: 200px;
	}
	.card-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.settings-card {
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
		padding: var(--md-sys-space-md);
	}
	.card-head {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-sm);
	}
	.card-title {
		font-size: 14px;
		font-weight: 600;
		color: var(--md-sys-color-primary);
	}
	.card-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin: 0;
		line-height: 1.4;
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
	.provider-title {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
		flex-wrap: wrap;
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
	.model-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-top: calc(-1 * var(--md-sys-space-sm)); margin-bottom: var(--md-sys-space-md); line-height: 1.45; }
	.model-hint code {
		background: var(--md-sys-color-surface-container-highest);
		padding: 1px 4px;
		border-radius: 4px;
	}
	.model-hint a {
		color: var(--md-sys-color-primary);
		text-decoration: none;
	}
	.model-hint a:hover {
		text-decoration: underline;
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
	.capability-block {
		margin-top: var(--md-sys-space-md);
		padding-top: var(--md-sys-space-md);
		border-top: 1px dashed var(--md-sys-color-outline-variant);
	}
	.capability-block.first {
		margin-top: 0;
		padding-top: 0;
		border-top: none;
	}
	.capability-block h4 {
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
	.range-value {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 40px;
		min-width: 44px;
		padding: 0 var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
	}
	@media (max-width: 700px) {
		.picker-card {
			grid-template-columns: 1fr;
			gap: var(--md-sys-space-md);
		}
	}
</style>