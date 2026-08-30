<script>
	import MaterialDialog from '$lib/MaterialDialog.svelte';

	let { open = false, stepNumber = null, isUserMessage = false, loading = false, onConfirm, onClose } = $props();

	let title = $derived(isUserMessage ? '回退并编辑消息' : '回退到上一步');
	let message = $derived(isUserMessage
		? `确定要回退到这条消息吗？消息内容将回到输入框，你可以编辑后重新发送。`
		: `确定要回退到第 ${stepNumber} 步吗？会话状态将回到该步骤，后续步骤将被丢弃。`);
	let confirmLabel = $derived(loading ? '处理中...' : '确认回退');
</script>

<MaterialDialog {open} onClose={loading ? undefined : onClose} {title}>
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
