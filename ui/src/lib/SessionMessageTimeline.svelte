<script lang="ts">
	import type { AgentMediaPlanPayload } from '$lib/contracts/agent.ts';
	import { fly } from 'svelte/transition';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import SessionWelcome from '$lib/SessionWelcome.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import SessionRunEndBanner from '$lib/SessionRunEndBanner.svelte';
	import ToolRunTimelineCard from '$lib/ToolRunTimelineCard.svelte';
	import { hasToolPreambleBefore } from '$lib/toolIntent.ts';
	import { renderedMessageKey } from '$lib/streaming.ts';
	import SessionActivityGroup from '$lib/SessionActivityGroup.svelte';
	import {
		firstWaitingBackgroundToolRunId,
		groupSessionTimeline,
		type AskMessageHandler,
		type AskSelectionChangeHandler,
		type SessionMessageContextMenuRequest,
	} from '$lib/sessionTimeline.ts';
	import type { SessionMessage, SessionRunEndStatus } from '$lib/sessionReducer.ts';
	import type { ToolRunPayload } from '$lib/contracts/toolRun.ts';

	interface Props {
		sessionId?: string | null;
		messages?: SessionMessage[];
		sessionToolRuns?: ToolRunPayload[];
		hotkeyBinding?: string;
		awaitingBackground?: boolean;
		awaitingBackgroundCount?: number;
		runEndStatus?: SessionRunEndStatus | null;
		runEndOutcome?: 'turn_completed' | null;
		runEndReason?: string;
		showContinueButton?: boolean;
		continueDisabled?: boolean;
		continueBusy?: boolean;
		onContextMenu?: (request: SessionMessageContextMenuRequest) => void;
		onAskSelectionChange?: AskSelectionChangeHandler;
		getAskSelection?: (messageId: string) => string[];
		onIgnore?: AskMessageHandler;
		onAskSubmit?: AskMessageHandler;
		onAskDismiss?: AskMessageHandler;
		onContinue?: () => void;
		mediaPlans?: AgentMediaPlanPayload[];
		confirmationAwaiting?: boolean;
		showModelSetup?: boolean;
		onWelcomePrompt?: (prompt: string) => void;
		onConfigureModel?: () => void;
	}

	let {
		sessionId = null,
		messages = [],
		sessionToolRuns = [],
		hotkeyBinding = 'Ctrl+Shift+Space',
		awaitingBackground = false,
		awaitingBackgroundCount = 0,
		runEndStatus = null,
		runEndOutcome = null,
		runEndReason = '',
		showContinueButton = false,
		continueDisabled = false,
		continueBusy = false,
		onContextMenu = () => {},
		onAskSelectionChange = () => {},
		getAskSelection = () => [],
		onIgnore = () => {},
		onAskSubmit = () => {},
		onAskDismiss = () => {},
		onContinue = () => {},
		mediaPlans = [],
		confirmationAwaiting = false,
		showModelSetup = false,
		onWelcomePrompt = () => {},
		onConfigureModel = () => {},
	}: Props = $props();

	let timelineItems = $derived(
		groupSessionTimeline(messages, {
			toolRuns: sessionToolRuns,
			awaitingBackground: awaitingBackground && !runEndStatus,
			awaitingBackgroundCount,
		}),
	);
	let awaitingBackgroundToolRunId = $derived(
		firstWaitingBackgroundToolRunId(sessionToolRuns, awaitingBackground && !runEndStatus),
	);
</script>

{#if timelineItems.length === 0 && !confirmationAwaiting}
	<SessionWelcome
		{hotkeyBinding}
		animated
		{showModelSetup}
		onSelectPrompt={onWelcomePrompt}
		{onConfigureModel}
	/>
{:else}
	<div class="message-list" role="log" aria-label="会话消息">
		{#each timelineItems as item (item.kind === 'message' ? renderedMessageKey(item.message) : item.id)}
			{#if item.kind === 'activity'}
				<SessionActivityGroup
					entries={item.entries}
					streaming={item.streaming}
					toolCount={item.toolCount}
					stepCount={item.stepCount}
					allMessages={messages}
					toolRuns={sessionToolRuns}
					{awaitingBackgroundToolRunId}
					{awaitingBackgroundCount}
					{mediaPlans}
					{sessionId}
					{onContextMenu}
					{onAskSelectionChange}
					{getAskSelection}
					{onIgnore}
					{onAskSubmit}
					{onAskDismiss}
				/>
			{:else if item.kind === 'tool_run'}
				<ToolRunTimelineCard
					toolRun={item.toolRun}
					awaitingBackgroundResult={item.awaitingBackgroundResult}
					awaitingBackgroundCount={item.awaitingBackgroundCount}
					showTerminalOutput={item.showTerminalOutput}
				/>
			{:else if item.kind === 'tool_run_wait'}
				<ToolRunTimelineCard
					awaitingBackgroundResult
					awaitingBackgroundCount={item.awaitingBackgroundCount}
				/>
			{:else}
				{@const msg = item.message}
				{@const showFallbackIntent =
					msg.type === 'tool' &&
					(msg.showFallbackIntent ?? !hasToolPreambleBefore(messages, item.index))}
				<ChatBubble
					sessionId={sessionId ?? undefined}
					role={msg.role ?? 'assistant'}
					content={msg.content ?? ''}
					type={msg.type}
					voice={msg.voice}
					time={msg.time}
					streaming={!!msg.streaming}
					toolName={msg.toolName ?? ''}
					outcome={msg.outcome ?? null}
					renderer={msg.renderer ?? null}
					result={msg.result}
					messageId={msg.id}
					stepNumber={msg.stepNumber}
					toolArgs={msg.toolArgs ?? null}
					toolCallPreview={msg.toolCallPreview ?? false}
					toolArgsStreaming={msg.toolArgsStreaming ?? false}
					toolArgsTruncated={msg.toolArgsTruncated ?? false}
					attachments={msg.attachments}
					{showFallbackIntent}
					options={msg.options ?? []}
					selectedAskOptions={getAskSelection(msg.id)}
					awaiting={msg.awaiting ?? false}
					received={msg.received ?? false}
					deliveryMode={msg.deliveryMode}
					deliveryAccepted={msg.deliveryAccepted ?? false}
					resolved={msg.resolved ?? null}
					toolRunId={msg.toolRunId ?? null}
					{onContextMenu}
					{onAskSelectionChange}
					{onIgnore}
					{onAskSubmit}
					{onAskDismiss}
				/>
			{/if}
		{/each}
	</div>
	{#if runEndStatus}
		<SessionRunEndBanner status={runEndStatus} outcome={runEndOutcome} reason={runEndReason} />
	{/if}
	{#if confirmationAwaiting}
		<div class="confirmation-wait-banner" role="status">
			<strong>等待权限确认</strong>
			<span>请在确认弹窗或“待处理”菜单中选择允许或拒绝，工具才会继续。</span>
		</div>
	{/if}
	{#if showContinueButton && !continueDisabled}
		<div class="continue-action" in:fly={{ y: 6, duration: 240 }}>
			<MaterialButton
				variant="outlined"
				className="continue-btn"
				ariaLabel="继续生成"
				ariaBusy={continueBusy}
				disabled={continueDisabled}
				title={continueDisabled ? '当前会话尚未进入可恢复状态' : '从上一条消息继续生成'}
				onclick={() => onContinue()}
			>
				<span>继续生成</span>
			</MaterialButton>
		</div>
	{/if}
{/if}

<style>
	.message-list {
		display: flex;
		flex-direction: column;
		align-items: stretch;
		width: 100%;
		gap: var(--md-sys-space-md);
	}

	.continue-action {
		display: flex;
		justify-content: flex-end;
		padding-top: var(--md-sys-space-xs);
	}
	.confirmation-wait-banner {
		display: flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs) var(--md-sys-space-sm);
		width: fit-content;
		max-width: 100%;
		margin: var(--md-sys-space-xs) auto 0;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-warning);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.confirmation-wait-banner strong {
		font-weight: 700;
	}
</style>
