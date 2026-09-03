<script>
	import MaterialDialog from './MaterialDialog.svelte';
	import ApiKeyField from './ApiKeyField.svelte';

	/**
	 * Shared API-key change dialog (Set / Change API Key).
	 * @prop {boolean} open
	 * @prop {string} label — what the key belongs to (shown in the hint)
	 * @prop {boolean} configured — drives the Set/Change title
	 * @prop {function(string): void} onConfirm — called with the non-empty key
	 * @prop {function(): void} onClose
	 */
	let { open = false, label = '', configured = false, onConfirm, onClose } = $props();

	let newKeyValue = $state('');

	$effect(() => {
		if (open) {
			newKeyValue = '';
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
	title={configured ? `Change ${label || 'API Key'}` : `Set ${label || 'API Key'}`}
>
	{#snippet children()}
		<p class="dialog-hint">Enter the key for <strong>{label}</strong>.</p>
		<ApiKeyField mode="edit" bind:value={newKeyValue} placeholder="sk-..." />
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={close}>Cancel</button>
		<button class="md-btn md-btn--filled" onclick={confirm} disabled={!newKeyValue.trim()}>Confirm</button>
	{/snippet}
</MaterialDialog>

<style>
	.dialog-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		margin-bottom: var(--md-sys-space-lg);
	}
</style>
