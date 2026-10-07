<script lang="ts">
	import type { AgentMediaPlanPayload } from '$lib/contracts/agent.ts';
	import { fly } from 'svelte/transition';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import Logo from '$lib/Logo.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import SessionRunEndBanner from '$lib/SessionRunEndBanner.svelte';
	import ToolRunTimelineCard from '$lib/ToolRunTimelineCard.svelte';
	import { hasToolPreambleBefore } from '$lib/toolIntent.ts';
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
		messages?: SessionMessage[];
		sessionToolRuns?: ToolRunPayload[];
		hotkeyBinding?: string;
		awaitingBackground?: boolean;
		awaitingBackgroundCount?: number;
		runEndStatus?: SessionRunEndStatus | null;
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
	}

	let {
		messages = [],
		sessionToolRuns = [],
		hotkeyBinding = 'Ctrl+Shift+Space',
		awaitingBackground = false,
		awaitingBackgroundCount = 0,
		runEndStatus = null,
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
	}: Props = $props();

	let timelineItems = $derived(
		groupSessionTimeline(messages, {
			toolRuns: sessionToolRuns,
			awaitingBackground: awaitingBackground && !runEndStatus,
			awaitingBackgroundCount,
		}),
	);
	let awaitingBackgroundToolRunId = $derived(
		firstWaitingBackgroundToolRunId(
			sessionToolRuns,
			awaitingBackground && !runEndStatus,
		),
	);
</script>

{#if timelineItems.length === 0}
	<div class="welcome" in:fly={{ y: 12, duration: 330 }}>
		<div class="welcome-mark"><Logo size={48} /></div>
		<h2>Haven</h2>
		<span class="welcome-kicker">本地 AI 助手</span>
		<p>输入任务，Haven 会在需要时调用本地工具协助完成。</p>
		<div class="welcome-hints" aria-label="可用输入方式">
			<span>直接输入</span>
			<span>{hotkeyBinding} 语音</span>
			<span>图片与文件</span>
		</div>
	</div>
{:else}
	<div class="message-list" role="log" aria-label="会话消息">
		{#each timelineItems as item (item.kind === 'message' ? item.message.id : item.id)}
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
					attachments={msg.attachments}
					{showFallbackIntent}
					options={msg.options ?? []}
					selectedAskOptions={getAskSelection(msg.id)}
					awaiting={msg.awaiting ?? false}
					received={msg.received ?? false}
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
		<SessionRunEndBanner
			status={runEndStatus}
			reason={runEndReason}
		/>
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
	.welcome {
		text-align: center;
		min-height: 100%;
		padding: var(--md-sys-space-3xl) var(--md-sys-space-lg);
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: var(--md-sys-space-md);
	}
	.welcome-mark {
		display: grid;
		place-items: center;
		width: 64px;
		height: 64px;
		border-radius: var(--md-sys-shape-medium);
		background: transparent;
	}
	.welcome h2 {
		font-family: var(--md-ref-typeface-brand);
		font-size: var(--md-sys-typescale-display-size);
		font-weight: 700;
		letter-spacing: 0;
		line-height: var(--md-sys-typescale-display-line-height);
		color: var(--md-sys-color-primary);
	}
	.welcome-kicker {
		margin-top: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.welcome p {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		max-width: 420px;
	}
	.welcome-hints {
		display: flex;
		align-items: center;
		justify-content: center;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs);
		max-width: 100%;
	}
	.welcome-hints span {
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		white-space: nowrap;
	}
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
</style>
