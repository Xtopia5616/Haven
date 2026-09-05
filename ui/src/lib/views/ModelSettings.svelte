<script>
	import logger from '$lib/logger.ts';
	import { invoke } from '$lib/tauri.ts';
	import { addNotification } from '$lib/stores.ts';
	import { formatError } from '$lib/formatError.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import RefreshButton from '$lib/RefreshButton.svelte';
	import ApiKeyField from '$lib/ApiKeyField.svelte';
	import MediaSettings from './MediaSettings.svelte';
	import { emptyRoleSlot, ensureRoleSlots, modelCards } from '$lib/modelRoles.ts';
	import { withNumberValue, withStringValue } from '$lib/typedCallbacks.js';
	import {
		API_STYLE_OPTIONS,
		apiStylePreset,
		applyProviderPreset,
		displayApiStyle,
		isKeylessProvider,
		isSttOnlyStyle,
		isTtsOnlyStyle,
	} from '$lib/apiStyle.ts';

	/**
	 * Provider and role model settings. Media channel configuration is delegated
	 * to MediaSettings, while this component remains the single owner of model
	 * discovery and provider/role mutation.
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

	const roleCards = modelCards;
	/** @param {string} key */
	function roleFor(key) {
		return (
			/** @type {any[]} */ (llmConfig.roles || []).find(
				(/** @type {any} */ role) => role.role === key,
			) || null
		);
	}
	/** @param {string} key */
	function ensureRole(key) {
		const existing = roleFor(key);
		if (existing) return existing;
		const slot = emptyRoleSlot(key);
		llmConfig.roles.push(slot);
		return slot;
	}
	/** @param {string} name */
	function providerByName(name) {
		return /** @type {any[]} */ (llmConfig.providers || []).find(
			(/** @type {any} */ provider) => provider.name === name,
		);
	}
	function providerOptions() {
		return [
			{ value: '', label: '未配置' },
			.../** @type {any[]} */ (llmConfig.providers || []).map(
				(/** @type {any} */ provider) => ({ value: provider.name, label: provider.name }),
			),
		];
	}
	/** @param {string} key @param {string} providerName */
	function setRoleProvider(key, providerName) {
		const slot = ensureRole(key);
		slot.provider = providerName;
		slot.model = '';
		slot.context_window = null;
		slot.cost_per_1k_input_tokens = null;
		slot.cost_per_1k_output_tokens = null;
		slot.cost_per_1k_cache_read_tokens = null;
		slot.cost_per_1k_cache_write_tokens = null;
		if (providerName) refreshProviderModels(providerName);
	}
	/** @param {any} slot @param {string} providerName @param {string} modelId @param {{ overwrite?: boolean }} [opts] */
	function applyDiscoveredModelMeta(slot, providerName, modelId, opts = {}) {
		const overwrite = !!opts.overwrite;
		const model = (modelsByProvider[providerName] || []).find(
			(/** @type {any} */ item) => item.id === modelId,
		);
		if (!model) {
			if (overwrite) {
				slot.context_window = null;
				slot.cost_per_1k_input_tokens = null;
				slot.cost_per_1k_output_tokens = null;
				slot.cost_per_1k_cache_read_tokens = null;
				slot.cost_per_1k_cache_write_tokens = null;
			}
			return;
		}
		/** @type {Record<string, any>} */
		const wrote = {};
		if (overwrite || slot.context_window == null || slot.context_window === 0) {
			const next = model.context_window > 0 ? model.context_window : null;
			if (slot.context_window !== next) {
				slot.context_window = next;
				wrote.context_window = next;
			}
		}
		if (overwrite || slot.cost_per_1k_input_tokens == null) {
			const next =
				typeof model.cost_per_1k_input_tokens === 'number'
					? model.cost_per_1k_input_tokens
					: null;
			if (slot.cost_per_1k_input_tokens !== next) {
				slot.cost_per_1k_input_tokens = next;
				wrote.cost_per_1k_input_tokens = next;
			}
		}
		if (overwrite || slot.cost_per_1k_output_tokens == null) {
			const next =
				typeof model.cost_per_1k_output_tokens === 'number'
					? model.cost_per_1k_output_tokens
					: null;
			if (slot.cost_per_1k_output_tokens !== next) {
				slot.cost_per_1k_output_tokens = next;
				wrote.cost_per_1k_output_tokens = next;
			}
		}
		return wrote;
	}
	function backfillRoleMetaFromDiscovery() {
		const fills = [];
		for (const slot of llmConfig.roles || []) {
			if (!slot?.provider || !slot?.model) continue;
			const wrote = applyDiscoveredModelMeta(slot, slot.provider, slot.model);
			if (wrote && Object.keys(wrote).length) fills.push({ role: slot.role, ...wrote });
		}
		onDiscoverySettled?.(fills);
	}
	/** @param {string} key @param {string} modelId */
	function setRoleModel(key, modelId) {
		const slot = ensureRole(key);
		slot.model = modelId;
		applyDiscoveredModelMeta(slot, slot.provider, modelId, { overwrite: true });
	}
	/** @param {string} providerName */
	function roleModelOptions(providerName) {
		return providerName
			? (modelsByProvider[providerName] || []).map((/** @type {any} */ model) => ({
					value: model.id,
					label: model.name || model.id,
				}))
			: [];
	}
	/** @param {string} providerName */
	function roleModelLoading(providerName) {
		return !!providerName && !!modelFetching[providerName];
	}
	/** @param {any} provider */
	function isProviderKeyConfigured(provider) {
		return (
			!!provider &&
			(provider.api_key ||
				keyConfiguredProviders[provider.name] ||
				isKeylessProvider(provider))
		);
	}
	/** @param {any} provider */
	function providerDisplayStyle(provider) {
		return displayApiStyle(provider);
	}

	/** provider name → fetched model list */
	/** @type {Record<string, any[]>} */
	let modelsByProvider = $state({});
	/** @type {Record<string, boolean>} */
	let modelFetching = $state({});
	let refreshingAll = $state(false);
	let lastRefreshNotify = $state(0);
	async function refreshAllModels(silent = false) {
		if (refreshingAll) return;
		const providers = /** @type {any[]} */ (llmConfig.providers || []).filter(
			(/** @type {any} */ provider) => provider.base_url.trim(),
		);
		if (providers.length === 0) return;
		refreshingAll = true;
		try {
			let failedProviders = [];
			if (providers.some((/** @type {any} */ provider) => provider.api_key)) {
				const results = await Promise.all(
					providers.map(async (/** @type {any} */ provider) => ({
						name: provider.name,
						ok: await refreshProviderModels(provider.name),
					})),
				);
				failedProviders = results
					.filter((result) => !result.ok)
					.map((result) => result.name);
			} else {
				modelsByProvider = (await invoke('discover_all_models')) || {};
			}
			backfillRoleMetaFromDiscovery();
			if (!silent && Date.now() - lastRefreshNotify > 2500) {
				lastRefreshNotify = Date.now();
				if (failedProviders.length > 0) {
					const label = failedProviders.join('、');
					addNotification(
						failedProviders.length === providers.length
							? `模型列表刷新失败：${label}`
							: `部分模型提供商刷新失败：${label}`,
						failedProviders.length === providers.length ? 'error' : 'warning',
						4000,
					);
				} else {
					addNotification('模型列表已刷新', 'success', 2500);
				}
			}
		} catch (e) {
			reportError(e, { context: 'ModelSettings', message: '刷新模型列表失败', log: false });
		} finally {
			refreshingAll = false;
		}
	}
	/** @param {string} providerName */
	async function refreshProviderModels(providerName) {
		const provider = providerByName(providerName);
		if (!provider || !provider.base_url.trim()) return false;
		if (modelFetching[providerName]) return false;
		modelFetching[providerName] = true;
		try {
			const list = await invoke('discover_models', {
				baseUrl: provider.base_url,
				apiKey: provider.api_key || '',
				provider: providerName,
			});
			modelsByProvider = { ...modelsByProvider, [providerName]: list || [] };
			backfillRoleMetaFromDiscovery();
		} catch (e) {
			logger.warn('ModelSettings', `discover_models ${providerName} error`, formatError(e));
			return false;
		} finally {
			modelFetching[providerName] = false;
		}
		return true;
	}
	let autoRefreshed = $state(false);
	$effect(() => {
		if (loaded && !autoRefreshed && (llmConfig.providers || []).length > 0) {
			autoRefreshed = true;
			refreshAllModels(true);
		}
	});
	let rolesInitialized = $state(false);
	$effect(() => {
		if (loaded && !rolesInitialized) {
			rolesInitialized = true;
			ensureRoleSlots(llmConfig.roles || []);
		}
	});

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
	/** @param {number} idx */
	function startEditProvider(idx) {
		const provider = llmConfig.providers[idx];
		providerDialog = {
			idx,
			form: {
				name: provider.name,
				api_style: providerDisplayStyle(provider),
				base_url: provider.base_url,
				api_key: '',
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
		const others = /** @type {any[]} */ (llmConfig.providers || []).filter(
			(/** @type {any} */ _, index) => index !== idx,
		);
		if (others.some((/** @type {any} */ provider) => provider.name === name)) {
			addNotification('Provider 名称已存在', 'error', 3000);
			return;
		}
		const previous = idx !== null ? llmConfig.providers[idx] : null;
		const preset = apiStylePreset(form.api_style);
		const provider = {
			...(previous || {}),
			name,
			provider: preset.provider,
			api_style: preset.api_style,
			base_url: form.base_url.trim(),
			api_key: form.api_key || previous?.api_key || '',
			auth_header_name: preset.auth_header_name,
			auth_header_prefix: preset.auth_header_prefix,
			proxy_url: previous?.proxy_url ?? null,
			no_proxy: previous?.no_proxy ?? null,
			default_max_tokens: previous?.default_max_tokens ?? null,
			default_temperature: previous?.default_temperature ?? null,
			default_timeout_secs:
				previous?.default_timeout_secs ?? (isSttOnlyStyle(preset.api_style) ? 30 : null),
			default_timeout_streaming_secs: previous?.default_timeout_streaming_secs ?? null,
			default_web_search: previous?.default_web_search ?? null,
		};
		if (idx === null) llmConfig.providers.push(provider);
		else {
			const oldName = llmConfig.providers[idx].name;
			llmConfig.providers[idx] = provider;
			if (oldName !== name) {
				for (const role of llmConfig.roles)
					if (role.provider === oldName) role.provider = name;
				if (stt?.provider === oldName) stt.provider = name;
				if (tts?.provider === oldName) tts.provider = name;
				if (imageGen?.provider === oldName) imageGen.provider = name;
				if (keyConfiguredProviders[oldName]) {
					delete keyConfiguredProviders[oldName];
					keyConfiguredProviders[name] = true;
				}
			}
		}
		if (provider.api_key || isKeylessProvider(provider)) keyConfiguredProviders[name] = true;
		providerDialog = { idx: null, form: null };
		addNotification('Provider 已保存', 'success', 2000);
		refreshAllModels(true);
	}
	/** @param {number} idx */
	function deleteProvider(idx) {
		const provider = llmConfig.providers[idx];
		if (!provider) return;
		for (const role of llmConfig.roles)
			if (role.provider === provider.name) {
				role.provider = '';
				role.model = '';
			}
		if (stt?.provider === provider.name) stt.provider = 'llm';
		if (tts?.provider === provider.name) tts.provider = 'none';
		if (imageGen?.provider === provider.name) imageGen.provider = 'none';
		llmConfig.providers.splice(idx, 1);
		delete modelsByProvider[provider.name];
		addNotification(`已删除 Provider ${provider.name}`, 'success', 2000);
	}
	/** @param {string} style */
	function applyApiStylePreset(style) {
		if (providerDialog?.form) applyProviderPreset(providerDialog.form, style);
	}
	/** @param {any} providerOrStyle */
	function apiStyleLabel(providerOrStyle) {
		const style =
			typeof providerOrStyle === 'string'
				? providerOrStyle
				: providerDisplayStyle(providerOrStyle);
		return API_STYLE_OPTIONS.find((option) => option.value === style)?.label || style || '自动';
	}
</script>

{#if section === 'media'}
	<MediaSettings
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
	/>
{/if}

{#if section === 'models'}
	<div class="section">
		<div class="llm-head">
			<h2>模型配置</h2>
			<div class="llm-head-actions">
				<RefreshButton
					label="刷新模型列表"
					loading={refreshingAll}
					onclick={() => refreshAllModels()}
					disabled={(llmConfig.providers || []).length === 0}
				/>
				<MaterialButton
					variant="outlined"
					label="添加 Provider"
					onclick={startAddProvider}
				/>
			</div>
		</div>
		<p class="model-hint">
			添加 Provider（地址 + API Key）后拉取 <code>/models</code
			>（含上下文长度、能力、定价等元数据）；选模型时自动填入 Context / 成本（若 Provider
			有返回）。媒体能力在「媒体」页配置。
		</p>
		{#if (llmConfig.providers || []).length === 0}<div class="providers-empty">
				<p class="model-hint">尚未配置任何 Provider。点击「添加 Provider」开始配置。</p>
			</div>{:else}<div class="providers-list">
				{#each llmConfig.providers as provider, idx (provider.name)}<div
						class="provider-card"
					>
						<div class="provider-main">
							<div class="provider-title">
								<span class="provider-name">{provider.name}</span><ApiKeyField
									mode="badge"
									configured={isProviderKeyConfigured(provider)}
									badgePrefix={apiStyleLabel(provider)}
								/>
							</div>
							<div class="provider-meta">
								<span class="provider-endpoint" title={provider.base_url}
									>{provider.base_url}</span
								>
								{#if modelsByProvider[provider.name]?.length}<span
										class="provider-models"
										>{modelsByProvider[provider.name].length} 个模型</span
									>{/if}
							</div>
						</div>
						<div class="provider-actions">
							<RefreshButton
								compact
								iconOnly
								loading={refreshingAll || !!modelFetching[provider.name]}
								title="刷新模型列表"
								onclick={() => refreshProviderModels(provider.name)}
							/>
							<MaterialIconButton
								icon="edit"
								label="编辑"
								title="编辑 Provider"
								onclick={() => startEditProvider(idx)}
							/>
							<MaterialIconButton
								variant="danger"
								icon="delete"
								label="删除"
								title="删除 Provider"
								onclick={() => deleteProvider(idx)}
							/>
						</div>
					</div>{/each}
			</div>{/if}
		<div class="card-list model-list">
			<div class="model-group">Core Models</div>
			{#each roleCards.filter((card) => card.group === 'core') as card}{@render rolePicker(
					card,
				)}{/each}
			<div class="model-group">Specialized Models</div>
			{#each roleCards.filter((card) => card.group === 'specialized') as card}{@render rolePicker(
					card,
				)}{/each}
		</div>
		<p class="cost-hint">
			Context / 成本优先用 Provider
			返回的元数据；均未填写时上下文回退到「限制」页的默认上下文窗口，成本按 0（不显示）。
		</p>
	</div>
{/if}

{#snippet rolePicker(card = /** @type {any} */ (null))}
	{@const slot = roleFor(card.key)}
	{#if slot}
		<div class="settings-card">
			<div class="picker-card">
				<div class="model-field model-role">
					<span class="field-label">{card.label}</span>
					<div class="role-hint">{card.hint}</div>
				</div>
				<div class="model-field">
					<span class="field-label">Provider</span><MaterialSelect
						id="{card.prefix}-provider"
						value={slot.provider}
						options={providerOptions()}
						onChange={withStringValue((v) => setRoleProvider(card.key, v))}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Model</span>{#if slot.provider}<MaterialAutocomplete
							id="{card.prefix}-model"
							value={slot.model}
							options={roleModelOptions(slot.provider)}
							placeholder={slot.model ? slot.model : '从获取的模型列表中选择或输入'}
							loading={roleModelLoading(slot.provider)}
							onChange={withStringValue((v) => setRoleModel(card.key, v))}
							onFocus={() => {
								if (!modelsByProvider[slot.provider]?.length)
									refreshProviderModels(slot.provider);
							}}
						/>{:else}<span class="provider-note">先选择 Provider</span>{/if}
				</div>
			</div>
			<div class="model-row overrides-row">
				<div class="model-field">
					<span class="field-label">Temp（可选）</span><MaterialNumberField
						id="{card.prefix}-temp"
						value={slot.temperature ?? 0.7}
						step={0.1}
						min={0}
						max={2}
						onChange={withNumberValue((v) => {
							slot.temperature = v;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Context K（可选）</span><MaterialNumberField
						id="{card.prefix}-context-window"
						value={slot.context_window != null && slot.context_window > 0
							? Math.round(slot.context_window / 1000)
							: 0}
						step={1}
						min={0}
						onChange={withNumberValue((v) => {
							slot.context_window = v > 0 ? Math.round(v * 1000) : null;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Cost $/1K in（可选）</span><MaterialNumberField
						id="{card.prefix}-cost-in"
						value={slot.cost_per_1k_input_tokens ?? 0}
						step={0.01}
						min={0}
						onChange={withNumberValue((v) => {
							slot.cost_per_1k_input_tokens = v;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Cost $/1K out（可选）</span><MaterialNumberField
						id="{card.prefix}-cost-out"
						value={slot.cost_per_1k_output_tokens ?? 0}
						step={0.01}
						min={0}
						onChange={withNumberValue((v) => {
							slot.cost_per_1k_output_tokens = v;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Cache read $/1K（可选）</span><MaterialNumberField
						id="{card.prefix}-cost-cache-read"
						value={slot.cost_per_1k_cache_read_tokens ?? 0}
						step={0.01}
						min={0}
						onChange={withNumberValue((v) => {
							slot.cost_per_1k_cache_read_tokens = v;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Cache write $/1K（可选）</span><MaterialNumberField
						id="{card.prefix}-cost-cache-write"
						value={slot.cost_per_1k_cache_write_tokens ?? 0}
						step={0.01}
						min={0}
						onChange={withNumberValue((v) => {
							slot.cost_per_1k_cache_write_tokens = v;
						})}
					/>
				</div>
			</div>
		</div>
	{/if}
{/snippet}

{#if providerDialog.form}
	{@const pdForm = providerDialog.form}
	<MaterialDialog
		open={true}
		title={providerDialog.idx === null ? '添加 Provider' : '编辑 Provider'}
		onClose={() => {
			providerDialog = { idx: null, form: null };
		}}
	>
		{#snippet children()}
			<div class="lib-form">
				<div class="model-field">
					<span class="field-label">名称</span><input
						type="text"
						class="md-input"
						bind:value={pdForm.name}
						placeholder="唯一名称，角色据此选择"
						autocomplete="off"
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Provider 预设</span><MaterialSelect
						id="prov-api-style"
						value={pdForm.api_style}
						options={API_STYLE_OPTIONS}
						onChange={withStringValue(applyApiStylePreset)}
					/>
				</div>
				{#if apiStylePreset(pdForm.api_style)}{@const preset = apiStylePreset(
						pdForm.api_style,
					)}
					<p class="model-hint">
						线协议 <code>{preset.api_style}</code>{#if preset.hint}
							— {preset.hint}{/if}{#if preset.docs_url || preset.console_url}
							{#if preset.docs_url}<a
									href={preset.docs_url}
									target="_blank"
									rel="noreferrer">文档</a
								>{/if}{#if preset.docs_url && preset.console_url}
								·
							{/if}{#if preset.console_url}<a
									href={preset.console_url}
									target="_blank"
									rel="noreferrer">控制台</a
								>{/if}{/if}
					</p>{/if}
				{#if isSttOnlyStyle(apiStylePreset(pdForm.api_style).api_style)}<p
						class="model-hint"
					>
						该协议仅支持语音转写。请将其分配给 Audio Model，并在「媒体」页语音卡片把 STT
						Provider 设为「音频模型」。
					</p>{:else if isTtsOnlyStyle(apiStylePreset(pdForm.api_style).api_style)}<p
						class="model-hint"
					>
						该协议仅支持语音合成。在「媒体」页语音卡片把 TTS Provider 设为此项，并填写
						Voice ID。
					</p>{/if}
				<div class="model-field">
					<span class="field-label">Base URL</span><input
						type="text"
						class="md-input"
						bind:value={pdForm.base_url}
						placeholder="https://api.openai.com/v1"
						autocomplete="off"
					/>
				</div>
				<div class="model-field">
					<span class="field-label">API Key</span><ApiKeyField
						mode="edit"
						bind:value={pdForm.api_key}
						configured={providerDialog.idx !== null &&
							isProviderKeyConfigured(llmConfig.providers[providerDialog.idx])}
						placeholder={providerDialog.idx === null ? 'sk-...' : ''}
					/>
				</div>
			</div>
		{/snippet}
		{#snippet footer()}
			<MaterialButton
				variant="text"
				label="取消"
				onclick={() => {
					providerDialog = { idx: null, form: null };
				}}
			/>
			<MaterialButton variant="filled" label="保存" onclick={saveProvider} />
		{/snippet}
	</MaterialDialog>
{/if}

<style>
	.section {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-xl);
		margin-bottom: var(--md-sys-space-xl);
	}
	.section h2 {
		font-size: var(--md-sys-typescale-title-medium-size);
		font-weight: 700;
		color: var(--md-sys-color-on-surface);
		letter-spacing: 0;
		line-height: var(--md-sys-typescale-title-medium-line-height);
		margin-bottom: var(--md-sys-space-lg);
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
	.llm-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
	}
	.llm-head h2 {
		margin: 0;
	}
	.llm-head-actions,
	.provider-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 0 0 auto;
		flex-shrink: 0;
		flex-wrap: nowrap;
	}
	.llm-head-actions {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		width: min(100%, 360px);
	}
	.llm-head-actions :global(.md-btn) {
		width: 100%;
		min-width: 0;
	}
	.provider-actions {
		justify-self: end;
	}
	.model-list {
		margin-top: var(--md-sys-space-lg);
	}
	.model-group {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-primary);
		margin: var(--md-sys-space-md) 0 var(--md-sys-space-xs);
	}
	.model-group:first-child {
		margin-top: 0;
	}
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
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
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
		flex-wrap: nowrap;
	}
	.provider-name {
		min-width: 0;
		flex: 1 1 auto;
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.provider-title :global(.api-key-badge) {
		flex: 0 0 auto;
	}
	.provider-meta {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
		overflow: hidden;
	}
	.provider-endpoint {
		min-width: 0;
		flex: 1 1 auto;
		overflow: hidden;
		text-overflow: ellipsis;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		white-space: nowrap;
	}
	.provider-models {
		flex: 0 0 auto;
		padding-left: var(--md-sys-space-xs);
		border-left: 1px solid var(--md-sys-color-outline-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-primary);
		white-space: nowrap;
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
		gap: var(--md-sys-space-xs);
	}
	.model-field .md-input {
		width: 100%;
	}
	.model-field :global(.md-number-field),
	.model-field :global(.md-select-container),
	.model-field :global(.ma-root) {
		width: 100%;
	}
	.field-label {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
	}
	.model-role .field-label {
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.role-hint,
	.provider-note {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 2px;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.provider-note {
		font-style: italic;
	}
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.model-hint code {
		background: var(--md-sys-color-surface-container-highest);
		padding: 1px 4px;
		border-radius: 4px;
	}
	.model-hint a {
		color: var(--md-sys-color-primary);
		text-decoration: none;
	}
	.cost-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-md);
		margin-bottom: 0;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	@media (max-width: 700px) {
		.llm-head {
			align-items: flex-start;
			flex-direction: column;
		}
		.llm-head-actions {
			width: 100%;
		}
		.picker-card {
			grid-template-columns: 1fr;
			gap: var(--md-sys-space-md);
		}
	}
	@media (max-width: 455px) {
		.provider-card {
			grid-template-columns: 1fr;
			align-items: start;
		}
		.provider-actions {
			width: 100%;
			justify-content: flex-end;
		}
	}
</style>
