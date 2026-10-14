<script lang="ts">
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import ApiKeyField from '$lib/ApiKeyField.svelte';
	import { withStringValue } from '$lib/typedCallbacks.ts';
	import {
		PROVIDER_PRESET_OPTIONS,
		providerPreset,
		mediaCapabilitiesSupportedByStyle,
		isSttOnlyStyle,
		isTtsOnlyStyle,
	} from '$lib/apiStyle.ts';
	import type { ProviderDialogForm, ProviderKeyCheckInput } from '$lib/settingsModelTypes.ts';

	interface Props {
		dialog: { idx: number | null; form: ProviderDialogForm | null };
		providers?: ProviderKeyCheckInput[];
		isProviderKeyConfigured: (provider: ProviderKeyCheckInput | undefined) => boolean;
		onClose: () => void;
		onSave: () => void | Promise<void>;
		onApplyApiStylePreset: (style: string) => void;
	}

	let {
		dialog = { idx: null, form: null },
		providers = [],
		isProviderKeyConfigured,
		onClose,
		onSave,
		onApplyApiStylePreset,
	}: Props = $props();
</script>

{#if dialog.form}
	{@const form = dialog.form}
	{@const mediaSupport = mediaCapabilitiesSupportedByStyle(form.api_style)}
	<MaterialDialog
		open={true}
		title={dialog.idx === null ? '添加 Provider' : '编辑 Provider'}
		{onClose}
	>
		{#snippet children()}
			<div class="lib-form">
				<div class="model-field">
					<span class="field-label">名称</span>
					<input
						type="text"
						class="md-input"
						bind:value={form.name}
						placeholder="唯一名称，角色据此选择"
						autocomplete="off"
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Provider 预设</span>
					<MaterialSelect
						id="prov-api-style"
						value={form.preset}
						options={PROVIDER_PRESET_OPTIONS}
						onChange={withStringValue(onApplyApiStylePreset)}
					/>
				</div>
				{#if providerPreset(form.preset)}
					{@const preset = providerPreset(form.preset)}
					<p class="model-hint">
						线协议 <code>{preset.api_style}</code>{#if preset.hint}
							— {preset.hint}{/if}
						{#if preset.docs_url || preset.console_url}
							{#if preset.docs_url}<a
									href={preset.docs_url}
									target="_blank"
									rel="noreferrer">文档</a
								>{/if}
							{#if preset.docs_url && preset.console_url}
								·
							{/if}
							{#if preset.console_url}<a
									href={preset.console_url}
									target="_blank"
									rel="noreferrer">控制台</a
								>{/if}
						{/if}
					</p>
				{/if}
				{#if isSttOnlyStyle(providerPreset(form.preset).api_style)}
					<p class="model-hint">
						该协议仅支持语音转写。请将其分配给 Audio Model，并在「媒体」页语音卡片把 STT
						Provider 设为「音频模型」。
					</p>
				{:else if isTtsOnlyStyle(providerPreset(form.preset).api_style)}
					<p class="model-hint">
						该协议仅支持语音合成。在「媒体」页语音卡片把 TTS Provider 设为此项，并填写
						Voice ID。
					</p>
				{/if}
				<div class="model-field">
					<span class="field-label">Base URL</span>
					<input
						type="text"
						class="md-input"
						bind:value={form.base_url}
						placeholder="https://api.openai.com/v1"
						autocomplete="off"
					/>
				</div>
				<fieldset class="media-capability-fieldset">
					<legend class="field-label">媒体端点</legend>
					<p class="model-hint">
						聊天协议兼容不代表服务提供这些独立端点。只为服务端确实实现的接口启用。
					</p>
					<div class="media-capability-row">
						<span>语音转写（STT）</span>
						<MaterialSwitch
							checked={form.media_capabilities.stt}
							disabled={!mediaSupport.stt}
							ariaLabel="启用语音转写端点"
							onChange={(checked) => (form.media_capabilities.stt = checked)}
						/>
					</div>
					<div class="media-capability-row">
						<span>语音合成（TTS）</span>
						<MaterialSwitch
							checked={form.media_capabilities.tts}
							disabled={!mediaSupport.tts}
							ariaLabel="启用语音合成端点"
							onChange={(checked) => (form.media_capabilities.tts = checked)}
						/>
					</div>
					<div class="media-capability-row">
						<span>图片生成</span>
						<MaterialSwitch
							checked={form.media_capabilities.image_gen}
							disabled={!mediaSupport.image_gen}
							ariaLabel="启用图片生成端点"
							onChange={(checked) => (form.media_capabilities.image_gen = checked)}
						/>
					</div>
				</fieldset>
				<div class="model-field">
					<span class="field-label">API Key</span>
					<ApiKeyField
						mode="edit"
						bind:value={form.api_key}
						configured={dialog.idx !== null &&
							isProviderKeyConfigured(providers[dialog.idx])}
						placeholder={dialog.idx === null ? 'sk-...' : ''}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">代理路由</span>
					<MaterialSelect
						id="prov-proxy-mode"
						ariaLabel="代理路由"
						value={form.proxy_mode}
						options={[
							{ value: 'system', label: '使用默认代理' },
							{ value: 'direct', label: '直连（忽略环境代理）' },
							{ value: 'custom', label: '指定代理' },
						]}
						onChange={withStringValue(
							(value) =>
								(form.proxy_mode = value as ProviderDialogForm['proxy_mode']),
						)}
					/>
				</div>
				{#if form.proxy_mode === 'system'}
					<p class="model-hint">
						跟随 Haven 启动环境中的代理设置。该 Provider
						的模型目录刷新和模型请求都会使用此路由。
					</p>
				{:else if form.proxy_mode === 'direct'}
					<p class="model-hint">
						绕过环境代理，直接连接此 Provider；适用于当前网络可直连的服务。
					</p>
				{:else}
					<div class="model-field">
						<span class="field-label">代理地址</span>
						<input
							type="text"
							class="md-input"
							bind:value={form.proxy_url}
							placeholder="http://127.0.0.1:7890"
							autocomplete="off"
						/>
					</div>
					<div class="model-field">
						<span class="field-label">绕过代理的主机</span>
						<input
							type="text"
							class="md-input"
							bind:value={form.no_proxy}
							placeholder="localhost, 127.0.0.1, .example.com"
							autocomplete="off"
						/>
						<p class="model-hint">
							可填多个主机名或
							IP，用逗号分隔；匹配的地址将直接连接。代理地址不支持内嵌账号密码。
						</p>
					</div>
				{/if}
				{#if dialog.idx === null}
					<p class="model-hint">
						添加后会立即尝试验证 API Key 并获取模型列表；失败时 Provider
						仍会添加，并提示后续处理方式。
					</p>
				{/if}
			</div>
		{/snippet}
		{#snippet footer()}
			<MaterialButton variant="text" label="取消" onclick={onClose} />
			<MaterialButton variant="filled" label="保存" onclick={onSave} />
		{/snippet}
	</MaterialDialog>
{/if}

<style>
	.lib-form {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
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
	.media-capability-fieldset {
		min-width: 0;
		margin: 0;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
	}
	.media-capability-fieldset legend {
		padding: 0 var(--md-sys-space-xs);
	}
	.media-capability-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		min-height: var(--md-comp-button-touch-height);
		font-size: var(--md-sys-typescale-body-medium-size);
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
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin: 0;
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
</style>
