<script lang="ts">
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import ApiKeyField from '$lib/ApiKeyField.svelte';
	import { withStringValue } from '$lib/typedCallbacks.ts';
	import {
		API_STYLE_OPTIONS,
		apiStylePreset,
		isSttOnlyStyle,
		isTtsOnlyStyle,
	} from '$lib/apiStyle.ts';

	type ProviderDraft = {
		name: string;
		api_style: string;
		base_url: string;
		api_key: string;
		proxy_mode: 'system' | 'direct' | 'custom';
		proxy_url: string;
		no_proxy: string;
	};
	type ProviderKeyStatus = {
		name: string;
		api_key?: string;
		api_key_ref?: string | null;
	};

	interface Props {
		dialog: { idx: number | null; form: ProviderDraft | null };
		providers?: ProviderKeyStatus[];
		isProviderKeyConfigured: (provider: ProviderKeyStatus | undefined) => boolean;
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
						value={form.api_style}
						options={API_STYLE_OPTIONS}
						onChange={withStringValue(onApplyApiStylePreset)}
					/>
				</div>
				{#if apiStylePreset(form.api_style)}
					{@const preset = apiStylePreset(form.api_style)}
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
				{#if isSttOnlyStyle(apiStylePreset(form.api_style).api_style)}
					<p class="model-hint">
						该协议仅支持语音转写。请将其分配给 Audio Model，并在「媒体」页语音卡片把 STT
						Provider 设为「音频模型」。
					</p>
				{:else if isTtsOnlyStyle(apiStylePreset(form.api_style).api_style)}
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
							(value) => (form.proxy_mode = value as ProviderDraft['proxy_mode']),
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
