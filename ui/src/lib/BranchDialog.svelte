<script>
	import MaterialDialog from '$lib/MaterialDialog.svelte';

	let { open = false, type = 'rollback', stepNumber = null, taskSummary = '', onConfirm, onClose } = $props();

	let title = $derived(type === 'rollback' ? '回退到上一步' : '创建分支');
	let message = $derived(type === 'rollback'
		? `确定要回退到第 ${stepNumber} 步吗？任务状态将回到该步骤，后续步骤将被丢弃。`
		: `确定要基于 "${taskSummary || '当前任务'}" 创建分支吗？系统将在新会话中复制任务状态，两者可独立演进。`);
</script>

<MaterialDialog {open} onClose {title}>
	{#snippet children()}
		<p class="dialog-text">{message}</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={onClose}>取消</button>
		<button class="md-btn md-btn--filled" onclick={onConfirm}>
			{type === 'rollback' ? '确认回退' : '创建分支'}
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
