<script>
	/**
	 * ConversationTimeline — semantic conversation boundary for streamed
	 * messages, tool results and ask/confirm interactions.
	 *
	 * The empty state is deliberately kept separate from the message renderer.
	 * ChatMessageTimeline pulls in markdown, syntax highlighting and tool-card
	 * components, none of which are needed for the first blank conversation.
	 */
	import ConversationEmptyState from './ConversationEmptyState.svelte';

	let { messages = [], ...restProps } = $props();
	/** @type {any} */
	let timelineComponent = $state(null);
	let timelineLoadState = $state('idle');
	/** @type {Promise<void> | null} */
	let timelineLoadPromise = null;

	function loadTimeline() {
		if (timelineComponent || timelineLoadPromise) return;
		timelineLoadState = 'loading';
		timelineLoadPromise = import('./ChatMessageTimeline.svelte')
			.then((module) => {
				timelineComponent = module.default;
				timelineLoadState = 'ready';
			})
			.catch(() => {
				timelineLoadState = 'error';
			})
			.finally(() => {
				timelineLoadPromise = null;
			});
	}

	function retryTimeline() {
		timelineLoadState = 'idle';
		loadTimeline();
	}

	$effect(() => {
		if (messages.length > 0) loadTimeline();
	});
</script>

{#if messages.length === 0}
	<ConversationEmptyState hotkeyBinding={restProps.hotkeyBinding} />
{:else if timelineComponent}
	{@const ChatTimeline = timelineComponent}
	<ChatTimeline {messages} {...restProps} />
{:else if timelineLoadState === 'error'}
	<div class="timeline-placeholder" role="alert">
		<span>会话内容暂时无法加载</span>
		<button class="md-btn md-btn--outlined" type="button" onclick={retryTimeline}>重试</button>
	</div>
{:else}
	<div class="timeline-placeholder" role="status" aria-live="polite" aria-busy="true">
		<span class="timeline-placeholder__dot" aria-hidden="true"></span>
		<span>正在加载会话…</span>
	</div>
{/if}

<style>
	.timeline-placeholder {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: var(--md-sys-space-sm);
		min-height: var(--md-sys-space-4xl);
		padding: var(--md-sys-space-2xl);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.timeline-placeholder__dot {
		width: 8px;
		height: 8px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		animation: timeline-placeholder-pulse 1.2s ease-in-out infinite;
	}
	@keyframes timeline-placeholder-pulse {
		0%,
		100% {
			opacity: 0.35;
		}
		50% {
			opacity: 1;
		}
	}
</style>
