<script lang="ts">
	import MaterialDialog from './MaterialDialog.svelte';
	import MaterialButton from './MaterialButton.svelte';
	import ApiKeyField from './ApiKeyField.svelte';

	interface Props {
		open?: boolean;
		label?: string;
		configured?: boolean;
		onConfirm?: (key: string) => void;
		onClose?: () => void;
	}

	/**
	 * Shared API-key change dialog (Set / Change API Key).
	 */
	let { open = false, label = '', configured = false, onConfirm, onClose }: Props = $props();

	let newKeyValue = $state('');

	$effect(() => {
		if (open) {
			newKeyValue = '';
		}
	});

	function close(): void {
		onClose?.();
	}

	function confirm(): void {
		if (newKeyValue.trim()) onConfirm?.(newKeyValue.trim());
	}
</script>

<MaterialDialog
	{open}
	onClose={close}
	title={configured ? `Change ${label || 'API Key'}` : `Set ${label || 'API Key'}`}
>
	{#snippet children()}
		<p class="dialog-hint">Enter the key for <strong>{label}</strong>.</p>
		<ApiKeyField mode="edit" bind:value={newKeyValue} placeholder="sk-..." />
	{/snippet}
	{#snippet footer()}
		<MaterialButton variant="text" label="Cancel" onclick={close} />
		<MaterialButton
			variant="filled"
			label="Confirm"
			onclick={confirm}
			disabled={!newKeyValue.trim()}
		/>
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
