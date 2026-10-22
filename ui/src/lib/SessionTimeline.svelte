<script lang="ts">
	import type { ComponentProps } from 'svelte';
	import SessionMessageTimeline from '$lib/SessionMessageTimeline.svelte';
	import SessionWelcome from '$lib/SessionWelcome.svelte';
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
		runEndOutcome = null,
		runEndReason = '',
		confirmationAwaiting = false,
		onWelcomePrompt = () => {},
		showModelSetup = false,
		onConfigureModel = () => {},
		...restProps
	}: Props = $props();
</script>

{#if loading}
	<LoadingState label="正在加载 Haven…" detail="正在准备你的工作区" />
{:else if messages.length === 0 && runEndStatus}
	<SessionRunEndBanner status={runEndStatus} outcome={runEndOutcome} reason={runEndReason} />
{:else if messages.length === 0 && sessionToolRuns.length === 0 && !awaitingBackground && !confirmationAwaiting}
	<SessionWelcome
		hotkeyBinding={restProps.hotkeyBinding}
		onSelectPrompt={onWelcomePrompt}
		showModelSetup={showModelSetup}
		{onConfigureModel}
	/>
{:else}
	<!-- Mount the populated timeline as soon as messages, ToolRuns, or a wait state exist. -->
	<SessionMessageTimeline
		{messages}
		{sessionToolRuns}
		{awaitingBackground}
		{runEndStatus}
		{runEndOutcome}
		{runEndReason}
		{confirmationAwaiting}
		{onWelcomePrompt}
		{showModelSetup}
		{onConfigureModel}
		{...restProps}
	/>
{/if}
