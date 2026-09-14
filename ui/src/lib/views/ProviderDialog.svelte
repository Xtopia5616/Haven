<script>
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import ApiKeyField from '$lib/ApiKeyField.svelte';
	import { withStringValue } from '$lib/typedCallbacks.js';
	import {
		API_STYLE_OPTIONS,
		apiStylePreset,
		isSttOnlyStyle,
		isTtsOnlyStyle,
	} from '$lib/apiStyle.ts';

	let {
		dialog = { idx: null, form: null },
		providers = [],
		isProviderKeyConfigured,
		onClose,
		onSave,
		onApplyApiStylePreset,
	} = $props();
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
