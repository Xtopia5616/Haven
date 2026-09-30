<script lang="ts">
	import LoadingState from './LoadingState.svelte';
	import MaterialButton from './MaterialButton.svelte';

	type AsyncStateName = 'loading' | 'empty' | 'error' | 'unconfigured';
	type AsyncStateLayout = 'centered' | 'compact';

	interface Props {
		state?: AsyncStateName;
		layout?: AsyncStateLayout;
		title?: string;
		message?: string;
		actionLabel?: string;
		onAction?: () => void;
	}

	let {
		state = 'empty',
		layout = 'centered',
		title = '',
		message = '',
		actionLabel = '',
		onAction = () => {},
	}: Props = $props();

	const icons: Record<AsyncStateName, string> = {
		loading: '…',
		empty: '✓',
		error: '!',
		unconfigured: '!',
	};
</script>

{#if state === 'loading'}
	<LoadingState label={title || '正在加载…'} detail={message} />
{:else}
	<section
		class="async-state md-card motion-surface-enter"
		data-state={state}
		data-layout={layout}
		role={state === 'error' ? 'alert' : 'status'}
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
	.async-state[data-state='error'] .async-state__icon {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.async-state[data-state='unconfigured'] .async-state__icon {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}
	.async-state[data-layout='compact'] {
		grid-template-columns: auto minmax(0, 1fr) auto;
		justify-items: stretch;
		align-items: center;
		gap: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-xl);
		padding: var(--md-sys-space-lg);
		text-align: left;
	}
	.async-state[data-state='unconfigured'][data-layout='compact'] {
		border-color: var(--md-sys-color-warning);
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}
	.async-state[data-layout='compact'] h2,
	.async-state[data-layout='compact'] p {
		grid-column: 2;
		margin: 0;
	}
	.async-state[data-layout='compact'] h2 {
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.async-state[data-state='unconfigured'][data-layout='compact'] p {
		color: var(--md-sys-color-on-warning-container);
	}
	.async-state[data-layout='compact'] h2 {
		align-self: end;
	}
	.async-state[data-layout='compact'] p {
		align-self: start;
	}
	.async-state[data-layout='compact'] .async-state__icon {
		grid-column: 1;
		grid-row: 1 / span 2;
	}
	.async-state[data-layout='compact'] :global(.md-btn) {
		grid-column: 3;
		grid-row: 1 / span 2;
	}
	.async-state p {
		max-width: 420px;
		color: var(--md-sys-color-on-surface-variant);
	}
	@media (max-width: 640px) {
		.async-state {
			padding-inline: var(--md-sys-space-lg);
		}
		.async-state[data-layout='compact'] {
			grid-template-columns: auto minmax(0, 1fr);
			align-items: start;
		}
		.async-state[data-layout='compact'] :global(.md-btn) {
			grid-column: 2;
			grid-row: auto;
			width: 100%;
		}
	}
</style>
