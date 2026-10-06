<script lang="ts">
	import type { ComponentProps } from 'svelte';
	import ChatMessageTimeline from '$lib/ChatMessageTimeline.svelte';
	import SessionEmptyState from './SessionEmptyState.svelte';
	import LoadingState from './LoadingState.svelte';
	import SessionTerminationBanner from './SessionTerminationBanner.svelte';

	type MessageTimelineProps = ComponentProps<typeof ChatMessageTimeline>;
	type Props = MessageTimelineProps & { loading?: boolean };

	/**
	 * SessionTimeline is the semantic boundary for streamed
	 * messages, tool results and ask/confirm interactions.
	 *
	 * The empty state is deliberately kept separate from the message renderer.
	 * ChatMessageTimeline pulls in markdown, syntax highlighting and tool-card
	 * components, none of which are needed for the first blank session.
	 */
	let {
		messages = [],
		sessionToolRuns = [],
		awaitingBackground = false,
		loading = false,
		terminationStatus = null,
		terminationReason = '',
		...restProps
	}: Props = $props();
</script>

{#if loading}
	<LoadingState label="正在加载 Haven…" detail="正在准备你的工作区" />
{:else if messages.length === 0 && terminationStatus}
	<SessionTerminationBanner status={terminationStatus} reason={terminationReason} />
{:else if messages.length === 0 && sessionToolRuns.length === 0 && !awaitingBackground}
	<SessionEmptyState hotkeyBinding={restProps.hotkeyBinding} />
{:else}
	<!-- Keep the message renderer available for the first streamed event. A
	     lazy component boundary here turns normal IPC latency into a loading
	     gap and can leave the conversation blank after a chunk-load failure. -->
	<ChatMessageTimeline
		{messages}
		{sessionToolRuns}
		{awaitingBackground}
		{terminationStatus}
		{terminationReason}
		{...restProps}
	/>
{/if}
