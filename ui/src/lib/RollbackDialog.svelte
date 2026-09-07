<script>
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';

	let {
		open = false,
		stepNumber = null,
		isUserMessage = false,
		loading = false,
		onConfirm,
		onClose,
	} = $props();

	let title = $derived(isUserMessage ? '回退并覆盖当前时间线' : '回退并覆盖当前时间线');
	let message = $derived(
		isUserMessage
			? `确定要回退到这条消息吗？当前消息及其后续内容会被删除，消息会回到输入框供你编辑后重新发送。`
			: `确定要回退到第 ${stepNumber} 步吗？当前时间线的后续步骤会被删除，回退后不会保留可切换的分支。`,
	);
	let confirmLabel = $derived(loading ? '处理中...' : '确认回退并覆盖');
</script>

<MaterialDialog {open} onClose={loading ? undefined : onClose} {title}>
	{#snippet children()}
		<p class="dialog-text">{message}</p>
	{/snippet}
	{#snippet footer()}
		<MaterialButton variant="text" label="取消" onclick={onClose} disabled={loading} />
		<MaterialButton
			variant="filled"
			label={confirmLabel}
			onclick={onConfirm}
			disabled={loading}
		/>
	{/snippet}
</MaterialDialog>

<style>
	.dialog-text {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
</style>
