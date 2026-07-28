<script>
	import MaterialDialog from '$lib/MaterialDialog.svelte';

	let { open = false, stepNumber = null, taskSummary = '', loading = false, onConfirm, onClose } = $props();

	let message = $derived(`确定要回退到第 ${stepNumber} 步吗？任务状态将回到该步骤，后续步骤将被丢弃。`);
	let confirmLabel = $derived(loading ? '处理中...' : '确认回退');
</script>

<MaterialDialog {open} onClose={loading ? undefined : onClose} title="回退到上一步">
	{#snippet children()}
		<p class="dialog-text">{message}</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={onClose} disabled={loading}>取消</button>
		<button class="md-btn md-btn--filled" onclick={onConfirm} disabled={loading}>
			{confirmLabel}
		</button>
	{/snippet}
</MaterialDialog>

<style>
	.dialog-text {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		line-height: 1.5;
	}
</style>
