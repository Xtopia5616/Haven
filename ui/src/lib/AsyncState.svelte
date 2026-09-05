<script>
	import LoadingState from './LoadingState.svelte';
	import MaterialButton from './MaterialButton.svelte';

	let {
		state = 'empty',
		title = '',
		message = '',
		actionLabel = '',
		onAction = () => {},
	} = $props();

	/** @type {Record<string, string>} */
	const icons = { loading: '…', empty: '✓', error: '!', unconfigured: '!' };
</script>

{#if state === 'loading'}
	<LoadingState label={title || '正在加载…'} detail={message} />
{:else}
	<section
		class="async-state md-card"
		data-state={state}
		aria-live={state === 'error' ? 'assertive' : 'polite'}
	>
		<span class="async-state__icon" aria-hidden="true">{icons[state] || '•'}</span>
		<h2>{title}</h2>
		{#if message}<p>{message}</p>{/if}
		{#if actionLabel}
			<MaterialButton variant="filled" label={actionLabel} onclick={() => onAction?.()} />
		{/if}
	</section>
{/if}

<style>
	.async-state {
		display: grid;
		justify-items: center;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-4xl) var(--md-sys-space-2xl);
		border: 1px dashed var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container);
		color: var(--md-sys-color-on-surface);
		text-align: center;
	}
	.async-state__icon {
		display: grid;
		place-items: center;
		width: var(--md-comp-button-touch-height);
		height: var(--md-comp-button-touch-height);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		font-size: 24px;
		font-weight: 700;
	}
	.async-state[data-state='error'] .async-state__icon,
	.async-state[data-state='unconfigured'] .async-state__icon {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.async-state p {
		max-width: 420px;
		color: var(--md-sys-color-on-surface-variant);
	}
	@media (max-width: 455px) {
		.async-state {
			padding-inline: var(--md-sys-space-lg);
		}
	}
</style>
