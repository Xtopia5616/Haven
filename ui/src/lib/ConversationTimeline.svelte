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
	import ChatMessageTimeline from './ChatMessageTimeline.svelte';

	let { messages = [], loading = false, ...restProps } = $props();
</script>

{#if loading}
	<LoadingState label="正在加载 Haven…" detail="正在准备你的工作区" />
{:else if messages.length === 0}
	<ConversationEmptyState hotkeyBinding={restProps.hotkeyBinding} />
{:else}
	<!-- Keep the message renderer available for the first streamed event. A
	     lazy component boundary here turns normal IPC latency into a loading
	     gap and can leave the conversation blank after a chunk-load failure. -->
	<ChatMessageTimeline {messages} {...restProps} />
{/if}
