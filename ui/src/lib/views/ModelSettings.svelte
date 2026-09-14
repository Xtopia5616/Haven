<script>
	import { addNotification } from '$lib/stores.ts';
	import MaterialCard from '$lib/MaterialCard.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import MediaSettings from './MediaSettings.svelte';
	import ProviderDialog from './ProviderDialog.svelte';
	import ProviderList from './ProviderList.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { createModelDiscovery } from '$lib/modelDiscovery.ts';
	import { emptyRoleSlot, ensureRoleSlots, modelCards } from '$lib/modelRoles.ts';
	import { withNumberValue, withStringValue } from '$lib/typedCallbacks.js';
	import {
		API_STYLE_OPTIONS,
		apiStylePreset,
		applyProviderPreset,
		displayApiStyle,
		isKeylessProvider,
		isSttOnlyStyle,
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
		mediaInputStrategy,
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
		if (providerName) discovery.refreshProviderModels(providerName);
	}
	/** @param {string} key @param {string} modelId */
	function setRoleModel(key, modelId) {
		const slot = ensureRole(key);
		slot.model = modelId;
		discovery.applyDiscoveredModelMeta(slot, slot.provider, modelId, { overwrite: true });
	}
	/** @param {string} providerName */
	function roleModelOptions(providerName) {
		return providerName ? discovery.modelOptions(providerName) : [];
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
	const discovery = createModelDiscovery({
		getProviders: () => llmConfig.providers || [],
		getRoles: () => llmConfig.roles || [],
		getModels: () => modelsByProvider,
		setModels: (models) => (modelsByProvider = models),
		isProviderFetching: (providerName) => !!modelFetching[providerName],
		isRefreshingAll: () => refreshingAll,
		setProviderFetching: (providerName, fetching) => {
			modelFetching = { ...modelFetching, [providerName]: fetching };
		},
		setRefreshingAll: (refreshing) => (refreshingAll = refreshing),
		onDiscoverySettled: (fills) => onDiscoverySettled?.(fills),
	});
	let autoRefreshed = $state(false);
	$effect(() => {
		if (loaded && !autoRefreshed && (llmConfig.providers || []).length > 0) {
			autoRefreshed = true;
			discovery.refreshAllModels(true);
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
	/** @param {string} providerName */
	function refreshProvider(providerName) {
		return discovery.refreshProviderModels(providerName);
	}
	/** @param {number | null | undefined} idx */
	function editProvider(idx) {
		if (idx == null) startAddProvider();
		else startEditProvider(idx);
	}
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
		discovery.refreshAllModels(true);
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
		const nextModels = { ...modelsByProvider };
		delete nextModels[provider.name];
		modelsByProvider = nextModels;
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
		{mediaInputStrategy}
		{contextLimits}
		{keyConfigured}
		{keyConfiguredProviders}
		{mcpServerNames}
	/>
{/if}

{#if section === 'models'}
	<SettingsSection className="model-section" ariaLabel="模型配置">
		<ProviderList
			providers={llmConfig.providers || []}
			{modelsByProvider}
			{modelFetching}
			{refreshingAll}
			{isProviderKeyConfigured}
			{apiStyleLabel}
			onRefreshAll={() => discovery.refreshAllModels()}
			onRefreshProvider={refreshProvider}
			onEditProvider={editProvider}
			onDeleteProvider={deleteProvider}
		/>
		<p class="model-hint">
			添加 Provider（地址 + API Key）后拉取 <code>/models</code
			>（含上下文长度、能力、定价等元数据）；选模型时自动填入 Context / 成本（若 Provider
			有返回）。媒体能力在「媒体」页配置。
		</p>
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
	</SettingsSection>
{/if}

{#snippet rolePicker(card = /** @type {any} */ (null))}
	{@const slot = roleFor(card.key)}
	{#if slot}
		<MaterialCard variant="outlined" className="settings-card">
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
									discovery.refreshProviderModels(slot.provider);
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
		</MaterialCard>
	{/if}
{/snippet}

<ProviderDialog
	dialog={providerDialog}
	providers={llmConfig.providers || []}
	{isProviderKeyConfigured}
	onClose={() => (providerDialog = { idx: null, form: null })}
	onSave={saveProvider}
	onApplyApiStylePreset={applyApiStylePreset}
/>

<style>
	.card-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	:global(.settings-card) {
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
		padding: var(--md-sys-space-md);
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
	.cost-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-md);
		margin-bottom: 0;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	@media (max-width: 700px) {
		.picker-card {
			grid-template-columns: 1fr;
			gap: var(--md-sys-space-md);
		}
	}
</style>
