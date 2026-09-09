<script>
	import MaterialButton from './MaterialButton.svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';
	import Icon from './Icon.svelte';

	/**
	 * Unified API-key control (same chrome as md-input).
	 *
	 * Modes:
	 * - `stored` — masked display + Set/Change; parent opens ApiKeyDialog via onEdit
	 * - `edit` — password input + visibility toggle; empty+configured keeps previous
	 * - `badge` — compact status chip for provider list rows
	 *
	 * @prop {'stored'|'edit'|'badge'} [mode='stored']
	 * @prop {boolean} [configured=false]
	 * @prop {string} [badgePrefix=''] — optional leading label in badge mode
	 * @prop {string} [value=''] — bindable in edit mode
	 * @prop {string} [id]
	 * @prop {string} [placeholder='sk-...']
	 * @prop {string} [keepHint='已配置，留空保持不变']
	 * @prop {boolean} [disabled=false]
	 * @prop {function(): void} [onEdit]
	 */
	let {
		mode = 'stored',
		configured = false,
		badgePrefix = '',
		value = $bindable(''),
		id = undefined,
		placeholder = 'sk-...',
		keepHint = '已配置，留空保持不变',
		disabled = false,
		onEdit = undefined,
	} = $props();

	const MASK = '••••••••••••••••';

	let showKey = $state(false);

	/** @type {string} */
	let editPlaceholder = $derived(configured && !value ? MASK : placeholder);

	function handleEdit() {
		if (disabled) return;
		onEdit?.();
	}
</script>

{#if mode === 'badge'}
	<span
		class="api-key-badge"
		class:configured
		class:empty={!configured}
		title={`${badgePrefix ? `${badgePrefix} · ` : ''}API Key ${configured ? '已配置' : '未配置'}`}
		aria-label={`${badgePrefix ? `${badgePrefix} · ` : ''}API Key ${configured ? '已配置' : '未配置'}`}
	>
		{#if badgePrefix}
			<span class="api-key-badge-prefix">{badgePrefix}</span>
			<span class="api-key-badge-divider" aria-hidden="true">·</span>
		{/if}
		{#if configured}
			<Icon name="checkmark" size={14} strokeWidth={2.5} className="api-key-badge-icon" />
		{:else}
			<Icon name="key" size={14} className="api-key-badge-icon" />
		{/if}
		<span class="api-key-badge-label">{configured ? '已配置' : '未配置'}</span>
	</span>
{:else if mode === 'edit'}
	<div class="api-key-field" class:empty={!configured && !value} class:disabled>
		<input
			{id}
			type={showKey ? 'text' : 'password'}
			class="api-key-input"
			bind:value
			placeholder={editPlaceholder}
			title={configured && !value ? keepHint : undefined}
			autocomplete="new-password"
			spellcheck="false"
			{disabled}
		/>
		<MaterialIconButton
			icon={showKey ? 'eyeOff' : 'eye'}
			className="api-key-icon-btn"
			label={showKey ? 'Hide API key' : 'Show API key'}
			title={showKey ? 'Hide API key' : 'Show API key'}
			{disabled}
			onclick={() => (showKey = !showKey)}
		/>
	</div>
{:else}
	<div class="api-key-field" class:empty={!configured} class:disabled>
		<MaterialButton
			variant="text"
			className="api-key-display"
			{id}
			title={configured ? 'Configured' : 'Not Configured'}
			ariaLabel={configured ? 'Change API key' : 'Set API key'}
			{disabled}
			onclick={handleEdit}
		>
			<span class="api-key-mask" class:grey={!configured}>{MASK}</span>
		</MaterialButton>
		<MaterialButton
			variant="text"
			className="api-key-action"
			label={configured ? 'Change' : 'Set'}
			{disabled}
			title={configured ? 'Configured' : 'Not Configured'}
			onclick={handleEdit}
		/>
	</div>
{/if}

<style>
	.api-key-field {
		display: flex;
		align-items: stretch;
		width: 100%;
		height: var(--md-comp-textfield-container-height);
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-comp-textfield-corner);
		background: transparent;
		overflow: hidden;
		box-sizing: border-box;
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.api-key-field:hover:not(.disabled) {
		border-color: var(--md-sys-color-on-surface);
	}
	.api-key-field:focus-within:not(.disabled) {
		border-color: var(--md-sys-color-primary);
		box-shadow: inset 0 0 0 1px var(--md-sys-color-primary);
	}
	.api-key-field.disabled {
		opacity: 0.55;
		pointer-events: none;
	}
	.api-key-field.empty {
		background: color-mix(in srgb, var(--md-sys-color-surface-container-high) 55%, transparent);
	}

	:global(.md-btn.api-key-display) {
		flex: 1;
		min-width: 0;
		display: flex;
		align-items: center;
		padding: 0 var(--md-sys-space-lg);
		border: none;
		background: transparent;
		cursor: pointer;
		text-align: left;
	}

	.api-key-mask {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		letter-spacing: 0.12em;
		color: var(--md-sys-color-on-surface);
		user-select: none;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.api-key-mask.grey {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.72;
	}

	.api-key-input {
		flex: 1;
		min-width: 0;
		height: 100%;
		border: none;
		outline: none;
		background: transparent;
		padding: 0 var(--md-sys-space-lg);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		letter-spacing: 0.04em;
		color: var(--md-sys-color-on-surface);
	}
	.api-key-input::placeholder {
		color: var(--md-sys-color-on-surface-variant);
		letter-spacing: 0.12em;
		opacity: 0.72;
	}

	:global(.md-btn.api-key-action) {
		flex-shrink: 0;
		min-width: 72px;
		padding: 0 var(--md-sys-space-md);
		border: none;
		border-left: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-primary);
		font-family: inherit;
		font-size: var(--md-sys-typescale-label-large-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-large-line-height);
		cursor: pointer;
		transition: background-color var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	:global(.md-btn.api-key-action:hover:not(:disabled)) {
		background: var(--md-sys-color-surface-container-high);
	}
	:global(.md-btn.api-key-action:disabled) {
		cursor: default;
	}

	:global(.md-icon-btn.api-key-icon-btn) {
		flex-shrink: 0;
		width: var(--md-comp-textfield-container-height);
		border: none;
		border-left: 1px solid var(--md-sys-color-outline-variant);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	:global(.md-icon-btn.api-key-icon-btn:hover:not(:disabled)) {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
	}

	.api-key-badge {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		height: 24px;
		padding: 0 var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-full);
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		max-width: 100%;
	}
	.api-key-badge.configured {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-success) 45%,
			var(--md-sys-color-outline-variant)
		);
		background: color-mix(
			in srgb,
			var(--md-sys-color-success) 12%,
			var(--md-sys-color-surface-container)
		);
		color: var(--md-sys-color-on-surface);
	}
	:global(.api-key-badge-icon) {
		width: 14px;
		height: 14px;
		flex: 0 0 auto;
	}
	.api-key-badge-prefix {
		min-width: 0;
		max-width: 14rem;
		overflow: hidden;
		text-overflow: ellipsis;
		font-weight: 500;
		white-space: nowrap;
	}
	.api-key-badge-divider {
		flex: 0 0 auto;
		opacity: 0.55;
	}
	.api-key-badge-label {
		font-weight: 600;
		white-space: nowrap;
	}
</style>
