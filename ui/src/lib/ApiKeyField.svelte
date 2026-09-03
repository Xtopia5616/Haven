<script>
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
	let editPlaceholder = $derived(
		configured && !value ? MASK : placeholder,
	);

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
		title={configured ? 'Configured' : 'Not Configured'}
	>
		<span class="api-key-badge-mask">{MASK.slice(0, 8)}</span>
		<span class="api-key-badge-label">{configured ? '已配置' : '未配置'}</span>
	</span>
{:else if mode === 'edit'}
	<div
		class="api-key-field"
		class:empty={!configured && !value}
		class:disabled
	>
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
		<button
			type="button"
			class="api-key-icon-btn"
			aria-label={showKey ? 'Hide API key' : 'Show API key'}
			title={showKey ? 'Hide API key' : 'Show API key'}
			disabled={disabled}
			onclick={() => { showKey = !showKey; }}
		>
			{#if showKey}
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" /><line x1="1" y1="1" x2="23" y2="23" /></svg>
			{:else}
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" /><circle cx="12" cy="12" r="3" /></svg>
			{/if}
		</button>
	</div>
{:else}
	<div
		class="api-key-field"
		class:empty={!configured}
		class:disabled
	>
		<button
			type="button"
			class="api-key-display"
			{id}
			title={configured ? 'Configured' : 'Not Configured'}
			aria-label={configured ? 'Change API key' : 'Set API key'}
			disabled={disabled}
			onclick={handleEdit}
		>
			<span class="api-key-mask" class:grey={!configured}>{MASK}</span>
		</button>
		<button
			type="button"
			class="api-key-action"
			disabled={disabled}
			title={configured ? 'Configured' : 'Not Configured'}
			onclick={handleEdit}
		>
			{configured ? 'Change' : 'Set'}
		</button>
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
		transition: border-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
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

	.api-key-display {
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

	.api-key-action {
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
	.api-key-action:hover:not(:disabled) {
		background: var(--md-sys-color-surface-container-high);
	}
	.api-key-action:disabled {
		cursor: default;
	}

	.api-key-icon-btn {
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
		transition: background-color var(--md-sys-motion-duration-fast)
				var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.api-key-icon-btn:hover:not(:disabled) {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
	}

	.api-key-badge {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		height: 22px;
		padding: 0 8px;
		border-radius: 999px;
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		max-width: 100%;
	}
	.api-key-badge.configured {
		border-color: color-mix(in srgb, var(--md-sys-color-success) 45%, var(--md-sys-color-outline-variant));
		background: color-mix(in srgb, var(--md-sys-color-success) 12%, var(--md-sys-color-surface-container));
		color: var(--md-sys-color-on-surface);
	}
	.api-key-badge-mask {
		font-family: var(--md-sys-typescale-mono);
		letter-spacing: 0.1em;
		opacity: 0.75;
	}
	.api-key-badge.empty .api-key-badge-mask {
		opacity: 0.45;
	}
	.api-key-badge-label {
		font-weight: 600;
		white-space: nowrap;
	}
</style>
