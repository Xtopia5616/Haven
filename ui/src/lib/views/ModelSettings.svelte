<script lang="ts">
	import { addNotification } from '$lib/notificationStore.ts';
	import MaterialCard from '$lib/MaterialCard.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MediaSettings from './MediaSettings.svelte';
	import ProviderDialog from './ProviderDialog.svelte';
	import ProviderList from './ProviderList.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { createModelDiscovery } from '$lib/modelDiscovery.ts';
	import { emptyModel, capabilityOptions, requestPolicyOptions } from '$lib/modelRoles.ts';
	import { withStringValue } from '$lib/typedCallbacks.ts';
	import type {
		ApiKeyStatus,
		AudioConfig,
		CapabilityInput,
		ContextLimitsConfigInput,
		ImageGenConfigInput,
		MediaInputStrategyInput,
		OcrConfigInput,
		RequestKindInput,
		SttConfigInput,
		TtsConfigInput,
	} from '$lib/contracts/generatedCommands.ts';
	import type { DiscoveredModelMap } from '$lib/contracts/model.ts';
	import type {
		ModelDraft,
		ProviderDraft,
		RequestPolicyDraft,
		SettingsLlmState,
	} from '$lib/settingsModelTypes.ts';
	type ProviderDialogForm = {
		name: string;
		api_style: string;
		base_url: string;
		api_key: string;
	};
	type OverrideField =
		| 'temperature'
		| 'context_window'
		| 'cost_per_1k_input_tokens'
		| 'cost_per_1k_output_tokens'
		| 'cost_per_1k_cache_read_tokens'
		| 'cost_per_1k_cache_write_tokens';

	interface Props {
		section?: 'models' | 'media';
		llmConfig: SettingsLlmState;
		audio: AudioConfig;
		stt: Required<SttConfigInput>;
		ocr: Required<OcrConfigInput>;
		tts: Required<TtsConfigInput>;
		imageGen: Required<ImageGenConfigInput>;
		mediaInputStrategy: MediaInputStrategyInput;
		contextLimits: Partial<ContextLimitsConfigInput>;
		keyConfigured: ApiKeyStatus;
		keyConfiguredProviders?: Record<string, boolean>;
		mcpServerNames?: string[];
		loaded?: boolean;
		onDiscoverySettled?: (fills: Array<Record<string, unknown>>) => void;
		onProviderDiscoveryFailure?: (providerName: string, staticCatalog: boolean) => void;
	}
	import {
		API_STYLE_OPTIONS,
		apiStylePreset,
		applyProviderPreset,
		displayApiStyle,
		isKeylessProvider,
		isSttOnlyStyle,
	} from '$lib/apiStyle.ts';

	/**
	 * Provider, model catalog, and request routing settings. Media channel configuration is delegated
	 * to MediaSettings, while this component remains the single owner of model
	 * discovery and configuration mutation.
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
		onProviderDiscoveryFailure = (_providerName, _staticCatalog) => {},
	}: Props = $props();

	function modelFor(id: string): ModelDraft | null {
		return llmConfig.models.find((model) => model.id === id) || null;
	}
	function addModel(providerName: string) {
		const base = 'model';
		let index = 1;
		while (modelFor(`${base}-${index}`)) index += 1;
		const model = emptyModel(`${base}-${index}`);
		model.provider = providerName;
		llmConfig.models.push(model);
		return model;
	}
	function removeModel(model: ModelDraft) {
		llmConfig.models = llmConfig.models.filter((item) => item !== model);
		for (const policy of llmConfig.request_policies) {
			if (policy.primary === model.id) policy.primary = '';
		}
	}
	function setModel(model: ModelDraft, modelId: string) {
		model.model = modelId;
		discovery.applyDiscoveredModelMeta(model, model.provider, modelId, { overwrite: true });
	}
	function setModelProvider(model: ModelDraft, providerName: string) {
		model.provider = providerName;
		model.model = '';
		model.context_window = null;
		model.cost_per_1k_input_tokens = null;
		model.cost_per_1k_output_tokens = null;
		model.cost_per_1k_cache_read_tokens = null;
		model.cost_per_1k_cache_write_tokens = null;
		if (providerName) discovery.refreshProviderModels(providerName);
	}
	function setCapability(model: ModelDraft, capability: CapabilityInput, checked: boolean) {
		const capabilities = Array.isArray(model.capabilities) ? model.capabilities : [];
		model.capabilities = checked
			? [...new Set([...capabilities, capability])]
			: capabilities.filter((item) => item !== capability);
	}
	const requestCapability: Record<RequestKindInput, CapabilityInput> = {
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
			(item) => !llmConfig.request_policies.some((policy) => policy.request === item.value),
		)?.value;
		if (!request) return;
		llmConfig.request_policies.push({ request, primary: '' });
	}
	function setPolicyRequest(policy: RequestPolicyDraft, request: RequestKindInput) {
		if (
			llmConfig.request_policies.some(
				(candidate) => candidate !== policy && candidate.request === request,
			)
		) {
			addNotification('每种 RequestKind 只能有一条策略', 'error', 3000);
			return;
		}
		policy.request = request;
		policy.primary = '';
	}
	function removePolicy(policy: RequestPolicyDraft) {
		llmConfig.request_policies = llmConfig.request_policies.filter((item) => item !== policy);
	}
	function modelLabel(model: ModelDraft) {
		const serviceModel = model.model || '尚未选择服务模型';
		return `${model.id} · ${model.provider || '未绑定 Provider'} / ${serviceModel}`;
	}
	function modelOptionsForPolicy(policy: RequestPolicyDraft) {
		const capability = requestCapability[policy.request];
		return [
			{ value: '', label: '未配置' },
			...llmConfig.models
				.filter(
					(candidate) =>
						!capability || (candidate.capabilities || []).includes(capability),
				)
				.map((candidate) => ({ value: candidate.id, label: modelLabel(candidate) })),
		];
	}
	function modelOptions(providerName: string) {
		return providerName ? discovery.modelOptions(providerName) : [];
	}
	function renameModel(model: ModelDraft, nextId: string) {
		const id = nextId.trim();
		if (!id || id === model.id) return;
		if (llmConfig.models.some((candidate) => candidate !== model && candidate.id === id)) {
			addNotification('Model ID 已存在', 'error', 3000);
			return;
		}
		const previousId = model.id;
		model.id = id;
		for (const policy of llmConfig.request_policies) {
			if (policy.primary === previousId) policy.primary = id;
		}
	}
	function updateModelOverride(model: ModelDraft, field: OverrideField, value: number | null) {
		model[field] = value;
	}
	type ProviderKeyStatus = {
		name: string;
		api_key?: string;
		api_key_ref?: string | null;
		api_style?: string | null;
		provider?: string | null;
	};
	function isProviderKeyConfigured(provider: ProviderKeyStatus | undefined) {
		return (
			!!provider &&
			(!!provider.api_key ||
				!!provider.api_key_ref ||
				keyConfiguredProviders[provider.name] ||
				isKeylessProvider(provider))
		);
	}
	function providerDisplayStyle(provider: ProviderDraft) {
		return displayApiStyle(provider);
	}

	let modelsByProvider = $state<DiscoveredModelMap>({});
	let modelFetching = $state<Record<string, boolean>>({});
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

	let providerDialog = $state<{ idx: number | null; form: ProviderDialogForm | null }>({
		idx: null,
		form: null,
	});
	function refreshProvider(providerName: string) {
		return discovery.refreshProviderModels(providerName);
	}
	function editProvider(idx: number | null | undefined) {
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
	function startEditProvider(idx: number) {
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
	async function saveProvider() {
		if (!providerDialog?.form) return;
		const { idx, form } = providerDialog;
		const name = form.name.trim();
		if (!name) {
			addNotification('请填写 Provider 名称', 'error', 3000);
			return;
		}
		const others = llmConfig.providers.filter((_, index) => index !== idx);
		if (others.some((provider) => provider.name === name)) {
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
			api_key: form.api_key.trim() || previous?.api_key || '',
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
		if (idx === null) {
			llmConfig.providers.push(provider);
			if (provider.api_key || isKeylessProvider(provider))
				keyConfiguredProviders[name] = true;
			providerDialog = { idx: null, form: null };

			const hasCredential = !!provider.api_key || isKeylessProvider(provider);
			const fetched = hasCredential
				? await discovery.refreshProviderModels(
						name,
						{
							authHeaderName: provider.auth_header_name,
							authHeaderPrefix: provider.auth_header_prefix,
							skipAuth: isKeylessProvider(provider),
						},
						false,
					)
				: false;
			if (fetched && !isSttOnlyStyle(provider.api_style)) {
				const count = modelsByProvider[name]?.length || 0;
				addNotification(`Provider 已添加并获取 ${count} 个模型`, 'success', 2500);
			} else {
				onProviderDiscoveryFailure?.(name, fetched && isSttOnlyStyle(provider.api_style));
			}
			return;
		} else {
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
	function deleteProvider(idx: number) {
		const provider = llmConfig.providers[idx];
		if (!provider) return;
		if (llmConfig.models.some((model) => model.provider === provider.name)) {
			addNotification(
				`请先移除 Provider「${provider.name}」下的模型，再删除该 Provider`,
				'error',
				3500,
			);
			return;
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
	function applyApiStylePreset(style: string) {
		if (providerDialog?.form) applyProviderPreset(providerDialog.form, style);
	}
	function apiStyleLabel(providerOrStyle: ProviderDraft | string) {
		const style =
			typeof providerOrStyle === 'string'
				? providerOrStyle
				: providerDisplayStyle(providerOrStyle);
		return API_STYLE_OPTIONS.find((option) => option.value === style)?.label || style || '自动';
	}
	function setPolicyRequestFromInput(policy: RequestPolicyDraft, value: string) {
		const request = requestPolicyOptions.find((option) => option.value === value)?.value;
		if (request) setPolicyRequest(policy, request);
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
			models={llmConfig.models || []}
			{modelsByProvider}
			{modelFetching}
			{refreshingAll}
			{modelOptions}
			{isProviderKeyConfigured}
			{apiStyleLabel}
			onRefreshAll={() => discovery.refreshAllModels()}
			onRefreshProvider={refreshProvider}
			onAddModel={addModel}
			onRenameModel={renameModel}
			onSetModel={setModel}
			onSetModelProvider={setModelProvider}
			onSetCapability={setCapability}
			onUpdateOverride={updateModelOverride}
			onRemoveModel={removeModel}
			onEditProvider={editProvider}
			onDeleteProvider={deleteProvider}
		/>
		<div class="policy-section-heading">
			<div>
				<h3>请求路由</h3>
				<p>
					每种请求类型使用一条策略，选择声明了对应能力的模型配置。音频与媒体服务另在「媒体」页设置。
				</p>
			</div>
			<StatusBadge label={`${llmConfig.request_policies?.length || 0} 条策略`} tone="info" />
		</div>
		<div class="card-list policy-list">
			{#each llmConfig.request_policies || [] as policy (policy.request)}
				<MaterialCard variant="outlined" className="settings-card policy-card">
					<div class="model-field">
						<span class="field-label">请求类型</span>
						<MaterialSelect
							id="policy-{policy.request}"
							value={policy.request}
							options={requestPolicyOptions}
							ariaLabel={`请求路由类型：${policy.request}`}
							onChange={withStringValue((value) =>
								setPolicyRequestFromInput(policy, value),
							)}
						/>
					</div>
					<div class="model-field">
						<span class="field-label">首选模型</span>
						<MaterialSelect
							id="policy-{policy.request}-primary"
							value={policy.primary || ''}
							options={modelOptionsForPolicy(policy)}
							ariaLabel={`${policy.request} 的首选模型`}
							onChange={withStringValue((value) => (policy.primary = value))}
						/>
					</div>
					<MaterialButton
						variant="text"
						label="移除策略"
						onclick={() => removePolicy(policy)}
					/>
				</MaterialCard>
			{/each}
			{#if !(llmConfig.request_policies || []).length}
				<div class="policy-empty">
					<strong>还没有请求路由</strong>
					<span>添加策略后，Haven 才能按请求类型选择已配置的模型。</span>
				</div>
			{/if}
			<div class="section-actions">
				<MaterialButton variant="outlined" label="添加请求路由" onclick={addPolicy} />
			</div>
		</div>
		<p class="cost-hint">
			模型上下文和成本默认读取 Provider
			目录元数据；没有元数据时可在模型的高级参数中填写。未配置上下文时回退到「限制」页的默认值。
		</p>
	</SettingsSection>
{/if}

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
	.policy-section-heading {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		margin-top: var(--md-sys-space-2xl);
		padding-top: var(--md-sys-space-xl);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.policy-section-heading h3 {
		margin: 0;
		font-size: var(--md-sys-typescale-title-large-size);
		line-height: var(--md-sys-typescale-title-large-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.policy-section-heading p {
		max-width: 680px;
		margin: var(--md-sys-space-xs) 0 0;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.policy-list {
		margin-top: var(--md-sys-space-md);
	}
	:global(.policy-card) {
		display: grid;
		grid-template-columns: minmax(180px, 0.8fr) minmax(260px, 1.4fr) auto;
		align-items: end;
		gap: var(--md-sys-space-md);
	}
	.section-actions {
		display: flex;
		justify-content: flex-end;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-sm);
	}
	.model-field {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.model-field :global(.md-number-field),
	.model-field :global(.md-number-field-with-unit),
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
	.policy-empty {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		padding: var(--md-sys-space-lg);
		border: 1px dashed var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.policy-empty strong {
		color: var(--md-sys-color-on-surface);
	}
	.cost-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-md);
		margin-bottom: 0;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	@media (max-width: 700px) {
		.policy-section-heading {
			align-items: flex-start;
			flex-direction: column;
		}
		:global(.policy-card) {
			grid-template-columns: 1fr;
		}
	}
</style>
