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
	import LoadingState from './LoadingState.svelte';
	import MaterialButton from './MaterialButton.svelte';
	import logger from './logger.ts';
	import { formatError } from './formatError.ts';

	let { messages = [], loading = false, ...restProps } = $props();
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
			.catch((error) => {
				logger.error('ConversationTimeline', 'message timeline load failed', formatError(error));
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

{#if loading}
	<LoadingState label="正在加载 Haven…" detail="正在准备你的工作区" />
{:else if messages.length === 0}
	<ConversationEmptyState hotkeyBinding={restProps.hotkeyBinding} />
{:else if timelineComponent}
	{@const ChatTimeline = timelineComponent}
	<ChatTimeline {messages} {...restProps} />
{:else if timelineLoadState === 'error'}
	<div class="timeline-placeholder" role="alert">
		<span>会话内容暂时无法加载</span>
		<MaterialButton variant="outlined" label="重试" onclick={retryTimeline} />
	</div>
{:else}
	<LoadingState label="正在加载会话…" detail="正在准备消息视图" variant="inline" />
{/if}

<style>
	.timeline-placeholder {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: var(--md-sys-space-md);
		min-height: var(--md-sys-space-4xl);
		padding: var(--md-sys-space-2xl);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		text-align: center;
	}
</style>
