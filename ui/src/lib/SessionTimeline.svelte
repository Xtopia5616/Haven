<script lang="ts">
	import type { ComponentProps } from 'svelte';
	import SessionMessageTimeline from '$lib/SessionMessageTimeline.svelte';
	import SessionEmptyState from './SessionEmptyState.svelte';
	import LoadingState from './LoadingState.svelte';
	import SessionRunEndBanner from './SessionRunEndBanner.svelte';

	type SessionMessageTimelineProps = ComponentProps<typeof SessionMessageTimeline>;
	type Props = SessionMessageTimelineProps & { loading?: boolean };

	/**
	 * SessionTimeline is the semantic boundary for streamed
	 * messages, tool results and ask/confirm interactions.
	 *
	 * Loading/empty presentation stays separate from rendering a populated
	 * session message timeline.
	 */
	let {
		messages = [],
		sessionToolRuns = [],
		awaitingBackground = false,
		loading = false,
		runEndStatus = null,
		runEndReason = '',
		...restProps
	}: Props = $props();
</script>

{#if loading}
	<LoadingState label="正在加载 Haven…" detail="正在准备你的工作区" />
{:else if messages.length === 0 && runEndStatus}
	<SessionRunEndBanner status={runEndStatus} reason={runEndReason} />
{:else if messages.length === 0 && sessionToolRuns.length === 0 && !awaitingBackground}
	<SessionEmptyState hotkeyBinding={restProps.hotkeyBinding} />
{:else}
	<!-- Mount the populated timeline as soon as messages, ToolRuns, or a wait state exist. -->
	<SessionMessageTimeline
		{messages}
		{sessionToolRuns}
		{awaitingBackground}
		{runEndStatus}
		{runEndReason}
		{...restProps}
	/>
{/if}
