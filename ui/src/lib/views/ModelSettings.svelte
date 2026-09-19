<script>
	import { addNotification } from '$lib/stores.ts';
	import MaterialCard from '$lib/MaterialCard.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MediaSettings from './MediaSettings.svelte';
	import ProviderDialog from './ProviderDialog.svelte';
	import ProviderList from './ProviderList.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { createModelDiscovery } from '$lib/modelDiscovery.ts';
	import { emptyModel, capabilityOptions, requestPolicyOptions } from '$lib/modelRoles.ts';
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

	/** @param {string} id */
	function modelFor(id) {
		return (
			/** @type {any[]} */ (llmConfig.models || []).find(
				(/** @type {any} */ model) => model.id === id,
			) || null
		);
	}
	function addModel() {
		const base = 'model';
		let index = 1;
		while (modelFor(`${base}-${index}`)) index += 1;
		const model = emptyModel(`${base}-${index}`);
		llmConfig.models.push(model);
		return model;
	}
	/** @param {any} model */
	function removeModel(model) {
		llmConfig.models = (llmConfig.models || []).filter((/** @type {any} */ item) => item !== model);
		for (const policy of llmConfig.request_policies || []) {
			if (policy.primary === model.id) policy.primary = '';
			policy.fallbacks = (policy.fallbacks || []).filter((/** @type {string} */ id) => id !== model.id);
		}
	}
	/** @param {any} model @param {string} providerName */
	function setModelProvider(model, providerName) {
		model.provider = providerName;
		model.model = '';
		model.context_window = null;
		model.cost_per_1k_input_tokens = null;
		model.cost_per_1k_output_tokens = null;
		model.cost_per_1k_cache_read_tokens = null;
		model.cost_per_1k_cache_write_tokens = null;
		if (providerName) discovery.refreshProviderModels(providerName);
	}
	/** @param {any} model @param {string} modelId */
	function setModel(model, modelId) {
		model.model = modelId;
		discovery.applyDiscoveredModelMeta(model, model.provider, modelId, { overwrite: true });
	}
	/** @param {any} model @param {string} capability @param {boolean} checked */
	function setCapability(model, capability, checked) {
		const capabilities = Array.isArray(model.capabilities) ? model.capabilities : [];
		model.capabilities = checked
			? [...new Set([...capabilities, capability])]
			: capabilities.filter((/** @type {string} */ item) => item !== capability);
	}
	/** @param {any} policy */
	/** @type {Record<string, string>} */
	const requestCapability = {
		chat: 'chat',
		fast_chat: 'fast_chat',
		vision: 'vision',
		audio_chat: 'audio_input',
		transcription: 'transcription',
		embedding: 'embedding',
		image_generation: 'image_generation',
		speech_synthesis: 'speech_synthesis',
	};
	function addPolicy() {
		const request = requestPolicyOptions.find(
			(item) => !(llmConfig.request_policies || []).some((/** @type {any} */ policy) => policy.request === item.value),
		)?.value;
		if (!request) return;
		llmConfig.request_policies.push({ request, primary: '', fallbacks: [] });
	}
	/** @param {any} policy @param {string} request */
	function setPolicyRequest(policy, request) {
		if (
			(llmConfig.request_policies || []).some(
				(/** @type {any} */ candidate) => candidate !== policy && candidate.request === request,
			)
		) {
			addNotification('每种 RequestKind 只能有一条策略', 'error', 3000);
			return;
		}
		policy.request = request;
		policy.primary = '';
		policy.fallbacks = [];
	}
	/** @param {any} policy */
	function removePolicy(policy) {
		llmConfig.request_policies = (llmConfig.request_policies || []).filter((/** @type {any} */ item) => item !== policy);
	}
	/** @param {any} model */
	function modelLabel(model) {
		return `${model.id}${model.model ? ` · ${model.model}` : ''}`;
	}
	/** @param {any} model */
	function modelOptionsForPolicy(model) {
		const capability = requestCapability[model?.request] || requestCapability[model];
		return [
			{ value: '', label: '未配置' },
			.../** @type {any[]} */ (llmConfig.models || [])
				.filter(
					(candidate) =>
						candidate !== model &&
						(!capability || (candidate.capabilities || []).includes(capability)),
				)
				.map((candidate) => ({ value: candidate.id, label: modelLabel(candidate) })),
		];
	}
	/** @param {any} model */
	function ensureModelShape(model) {
		if (!Array.isArray(model.capabilities)) model.capabilities = [];
		return model;
	}
	/** @param {any} model */
	function isAssigned(model) {
		return !!model?.provider && !!model?.model;
	}
	function providerOptions() {
		return [
			{ value: '', label: '未配置' },
			.../** @type {any[]} */ (llmConfig.providers || []).map(
				(/** @type {any} */ provider) => ({ value: provider.name, label: provider.name }),
			),
		];
	}
	/** @param {string} providerName */
	function modelOptions(providerName) {
		return providerName ? discovery.modelOptions(providerName) : [];
	}
	/** @param {string} providerName */
	function modelLoading(providerName) {
		return !!providerName && !!modelFetching[providerName];
	}
	/** @param {any} model @param {string} nextId */
	function renameModel(model, nextId) {
		const id = nextId.trim();
		if (!id || id === model.id) return;
		if (
			(llmConfig.models || []).some(
				(/** @type {any} */ candidate) => candidate !== model && candidate.id === id,
			)
		) {
			addNotification('Model ID 已存在', 'error', 3000);
			return;
		}
		const previousId = model.id;
		model.id = id;
		for (const policy of llmConfig.request_policies || []) {
			if (policy.primary === previousId) policy.primary = id;
			policy.fallbacks = (policy.fallbacks || []).map((/** @type {string} */ candidate) =>
				candidate === previousId ? id : candidate,
			);
		}
	}
	/** @param {any} policy @param {number} index @param {string} value */
	function setPolicyFallback(policy, index, value) {
		const fallbacks = [...(policy.fallbacks || [])];
		if (value) fallbacks[index] = value;
		else fallbacks.splice(index, 1);
		policy.fallbacks = fallbacks;
	}
	/** @param {any} policy */
	function addPolicyFallback(policy) {
		const options = modelOptionsForPolicy(policy).filter(
			(option) => option.value && !(policy.fallbacks || []).includes(option.value),
		);
		if (options.length > 0) policy.fallbacks = [...(policy.fallbacks || []), ''];
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
		getModels: () => llmConfig.models || [],
		getDiscoveredModels: () => modelsByProvider,
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
				for (const model of llmConfig.models)
					if (model.provider === oldName) model.provider = name;
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
		for (const model of llmConfig.models)
			if (model.provider === provider.name) {
				model.provider = '';
				model.model = '';
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
		<div class="section-actions">
			<MaterialButton variant="outlined" label="添加模型" onclick={addModel} />
		</div>
		<div class="card-list model-list">
			{#each llmConfig.models || [] as model, index (model.id)}
				{@render modelPicker(ensureModelShape(model), index)}
			{:else}<p class="provider-note">尚未配置模型。先添加一个模型，再为请求策略选择 capability。</p>{/each}
		</div>
		<div class="model-group">Request policies</div>
		<p class="model-hint">请求策略声明能力需求与候选模型顺序；不再通过专用 slot 或布尔开关决定 STT/视觉路由。</p>
		<div class="card-list policy-list">
			{#each llmConfig.request_policies || [] as policy (policy.request)}
				<MaterialCard variant="outlined" className="settings-card policy-card">
					<div class="model-field">
						<span class="field-label">请求</span>
						<MaterialSelect
							id="policy-{policy.request}"
							value={policy.request}
							options={requestPolicyOptions}
							onChange={withStringValue((value) => setPolicyRequest(policy, value))}
						/>
					</div>
					<div class="model-field">
						<span class="field-label">Primary</span>
						<MaterialSelect
							id="policy-{policy.request}-primary"
							value={policy.primary || ''}
							options={modelOptionsForPolicy(policy)}
							onChange={withStringValue((value) => (policy.primary = value))}
						/>
					</div>
					<div class="fallback-list">
						<span class="field-label">Fallbacks（按顺序）</span>
						{#each policy.fallbacks || [] as fallback, fallbackIndex}
							<div class="fallback-row">
								<MaterialSelect
									id="policy-{policy.request}-fallback-{fallbackIndex}"
									value={fallback}
									options={modelOptionsForPolicy(policy)}
									onChange={withStringValue((value) => setPolicyFallback(policy, fallbackIndex, value))}
								/>
								<MaterialButton
									variant="text"
									label="移除"
									onclick={() => setPolicyFallback(policy, fallbackIndex, '')}
								/>
							</div>
						{/each}
						<MaterialButton
							variant="text"
							label="添加 fallback"
							onclick={() => addPolicyFallback(policy)}
						/>
					</div>
					<MaterialButton variant="text" label="移除" onclick={() => removePolicy(policy)} />
				</MaterialCard>
			{/each}
			<div class="section-actions">
				<MaterialButton variant="outlined" label="添加请求策略" onclick={addPolicy} />
			</div>
		</div>
		<p class="cost-hint">
			Context / 成本优先用 Provider
			返回的元数据；均未填写时上下文回退到「限制」页的默认上下文窗口，成本按 0（不显示）。
		</p>
</SettingsSection>
{/if}

{#snippet modelPicker(model = /** @type {any} */ (null), index = 0)}
		<MaterialCard variant="outlined" className="settings-card">
			<div class="picker-card">
				<div class="model-field model-role">
					<span class="field-label">Model {index + 1}</span>
					<div class="role-hint">{model.id} · {isAssigned(model) ? model.model : '未配置'}</div>
					<input
						class="model-id"
						value={model.id}
						onchange={(event) => renameModel(model, event.currentTarget.value)}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Provider</span><MaterialSelect
						id="model-{index}-provider"
						value={model.provider}
						options={providerOptions()}
						onChange={withStringValue((v) => setModelProvider(model, v))}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Model</span>{#if model.provider}<MaterialAutocomplete
							id="model-{index}-name"
							value={model.model}
							options={modelOptions(model.provider)}
							placeholder={model.model ? model.model : '从获取的模型列表中选择或输入'}
							loading={modelLoading(model.provider)}
							onChange={withStringValue((v) => setModel(model, v))}
							onFocus={() => {
								if (!modelsByProvider[model.provider]?.length)
									discovery.refreshProviderModels(model.provider);
							}}
						/>{:else}<span class="provider-note">先选择 Provider</span>{/if}
				</div>
			</div>
			<div class="capability-list">
				<span class="field-label">Capabilities</span>
				{#each capabilityOptions as capability}
					<label class="capability-option">
						<input
							type="checkbox"
							checked={(model.capabilities || []).includes(capability.value)}
							onchange={(event) => setCapability(model, capability.value, event.currentTarget.checked)}
						/>{capability.label}
					</label>
				{/each}
			</div>
			<div class="model-row overrides-row">
				<div class="model-field">
					<span class="field-label">Temp（可选）</span><MaterialNumberField
						id="model-{index}-temp"
						value={model.temperature ?? 0.7}
						step={0.1}
						min={0}
						max={2}
						onChange={withNumberValue((v) => {
							model.temperature = v;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Context K（可选）</span><MaterialNumberField
						id="model-{index}-context-window"
						value={model.context_window != null && model.context_window > 0
							? Math.round(model.context_window / 1000)
							: 0}
						step={1}
						min={0}
						onChange={withNumberValue((v) => {
							model.context_window = v > 0 ? Math.round(v * 1000) : null;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Cost $/1K in（可选）</span><MaterialNumberField
						id="model-{index}-cost-in"
						value={model.cost_per_1k_input_tokens ?? 0}
						step={0.01}
						min={0}
						onChange={withNumberValue((v) => {
							model.cost_per_1k_input_tokens = v;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Cost $/1K out（可选）</span><MaterialNumberField
						id="model-{index}-cost-out"
						value={model.cost_per_1k_output_tokens ?? 0}
						step={0.01}
						min={0}
						onChange={withNumberValue((v) => {
							model.cost_per_1k_output_tokens = v;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Cache read $/1K（可选）</span><MaterialNumberField
						id="model-{index}-cost-cache-read"
						value={model.cost_per_1k_cache_read_tokens ?? 0}
						step={0.01}
						min={0}
						onChange={withNumberValue((v) => {
							model.cost_per_1k_cache_read_tokens = v;
						})}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Cache write $/1K（可选）</span><MaterialNumberField
						id="model-{index}-cost-cache-write"
						value={model.cost_per_1k_cache_write_tokens ?? 0}
						step={0.01}
						min={0}
						onChange={withNumberValue((v) => {
							model.cost_per_1k_cache_write_tokens = v;
						})}
					/>
				</div>
			</div>
		</MaterialCard>
		<MaterialButton variant="text" label="移除模型" onclick={() => removeModel(model)} />
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
	.policy-list {
		margin-top: var(--md-sys-space-sm);
	}
	.section-actions {
		display: flex;
		justify-content: flex-end;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-sm);
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
	.model-id {
		width: 100%;
		box-sizing: border-box;
		padding: var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-extra-small);
		background: var(--md-sys-color-surface-container-lowest);
		color: var(--md-sys-color-on-surface);
	}
	.capability-list,
	.fallback-list {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-md);
	}
	.capability-list .field-label,
	.fallback-list .field-label {
		width: 100%;
	}
	.capability-option {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.fallback-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		width: min(100%, 360px);
	}
	.fallback-row :global(.md-select-container) {
		flex: 1;
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
