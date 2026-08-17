<script>
	import MaterialDialog from './MaterialDialog.svelte';

	/**
	 * Shared API-key change dialog (Set / Change API Key). Used by the model
	 * role cards, the model library, and the media-capability cards; each
	 * caller passes its own target (`model`), the display `label`, and an
	 * `onConfirm` that writes the entered key into its config state.
	 * @prop {boolean} open
	 * @prop {string} label — what the key belongs to (shown in the hint)
	 * @prop {boolean} configured — drives the Set/Change title
	 * @prop {function(string): void} onConfirm — called with the non-empty key
	 * @prop {function(): void} onClose
	 */
	let { open = false, label = '', configured = false, onConfirm, onClose } = $props();

	let newKeyValue = $state('');
	let showKey = $state(false);

	$effect(() => {
		if (open) {
			newKeyValue = '';
			showKey = false;
		}
	});

	function close() {
		onClose?.();
	}

	function confirm() {
		if (newKeyValue.trim()) onConfirm?.(newKeyValue.trim());
	}
</script>

<MaterialDialog
	open={open}
	onClose={close}
	title={configured ? 'Change API Key' : 'Set API Key'}
>
	{#snippet children()}
		<p class="dialog-hint">Enter the API key for <strong>{label}</strong>.</p>
		<div class="key-input-row">
			<input
				type={showKey ? 'text' : 'password'}
				class="md-input"
				bind:value={newKeyValue}
				placeholder="sk-..."
				autocomplete="new-password"
			/>
			<button
				class="key-visibility-btn"
				type="button"
				aria-label={showKey ? 'Hide API key' : 'Show API key'}
				title={showKey ? 'Hide API key' : 'Show API key'}
				onclick={() => { showKey = !showKey; }}
			>
				{#if showKey}
					<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" /><line x1="1" y1="1" x2="23" y2="23" /></svg>
				{:else}
					<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" /><circle cx="12" cy="12" r="3" /></svg>
				{/if}
			</button>
		</div>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={close}>Cancel</button>
		<button class="md-btn md-btn--filled" onclick={confirm}>Confirm</button>
	{/snippet}
</MaterialDialog>

<style>
	.dialog-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		line-height: 1.5;
		margin-bottom: var(--md-sys-space-lg);
	}
	.key-input-row { display: flex; align-items: center; gap: var(--md-sys-space-xs); }
	.key-input-row .md-input { flex: 1; min-width: 0; }
	.key-visibility-btn {
		background: none;
		border: 1px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
		width: 38px;
		height: 38px;
		border-radius: var(--md-sys-shape-small);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		flex-shrink: 0;
		transition: background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.key-visibility-btn:hover {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
	}
</style>