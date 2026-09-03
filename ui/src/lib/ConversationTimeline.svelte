<script>
	/**
	 * ConversationTimeline — semantic conversation boundary for streamed
	 * messages, tool results and ask/confirm interactions.
	 */
	import ChatMessageTimeline from './ChatMessageTimeline.svelte';
	let { ...restProps } = $props();
</script>

<ChatMessageTimeline {...restProps} />
