<script>
	import { fly } from 'svelte/transition';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import Logo from '$lib/Logo.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import { hasToolPreambleBefore } from '$lib/toolIntent.ts';
	import { isFirstToolForStep } from '$lib/sessionUsagePresentation.ts';

	let {
		messages = [],
		hotkeyBinding = 'Ctrl+Shift+Space',
		awaitingBackground = false,
		awaitingBackgroundCount = 0,
		activeSessionError = false,
		showContinueButton = false,
		continueDisabled = false,
		continueBusy = false,
		stepUsage = () => null,
		onContextMenu = () => {},
		onAskSelectionChange = () => {},
		onIgnore = () => {},
		onAskSubmit = () => {},
		onContinue = () => {},
	} = $props();
</script>

{#if messages.length === 0}
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
		{#each messages as msg, index (msg.id)}
			{@const showFallbackIntent =
				msg.type === 'tool' &&
				(msg.showFallbackIntent ?? !hasToolPreambleBefore(messages, index))}
			{@const showStepUsage = isFirstToolForStep(messages, index)}
			<ChatBubble
				role={msg.role}
				content={msg.content}
				type={msg.type}
				voice={msg.voice}
				time={msg.time}
				streaming={!!msg.streaming}
				toolName={msg.toolName ?? ''}
				unrecoverable={!!msg.unrecoverable}
				outcome={msg.outcome ?? null}
				messageId={msg.id}
				stepNumber={msg.stepNumber}
				usage={showStepUsage ? stepUsage(msg.stepNumber) : null}
				toolArgs={msg.toolArgs ?? null}
				attachments={msg.attachments}
				{showFallbackIntent}
				options={msg.options ?? []}
				awaiting={msg.awaiting ?? false}
				received={msg.received ?? false}
				resolved={msg.resolved ?? null}
				actionId={msg.actionId ?? null}
				{onContextMenu}
				{onAskSelectionChange}
				{onIgnore}
				{onAskSubmit}
			/>
		{/each}
	</div>
	{#if showContinueButton}
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
				<svg
					width="16"
					height="16"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
					aria-hidden="true"
				>
					<path d="M20 11a8 8 0 1 0 2 5.3" />
					<polyline points="20 4 20 11 13 11" />
				</svg>
				<span>继续生成</span>
			</MaterialButton>
		</div>
	{/if}
{/if}

{#if awaitingBackground && !activeSessionError}
	<div class="awaiting-bg-banner" in:fly={{ y: 8, duration: 300 }} role="status">
		<span class="awaiting-bg-dot" aria-hidden="true"></span>
		<span class="awaiting-bg-text">
			等待后台任务结果{#if awaitingBackgroundCount > 1}（{awaitingBackgroundCount}）{/if}，完成后将自动继续
		</span>
	</div>
{/if}

<style>
	.welcome {
		text-align: center;
		min-height: min(420px, 100%);
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
		gap: var(--md-sys-space-md);
	}

	.continue-action {
		display: flex;
		justify-content: flex-end;
		padding-top: var(--md-sys-space-xs);
	}
	:global(.continue-btn) {
		gap: var(--md-sys-space-xs);
	}

	.awaiting-bg-banner {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		max-width: min(800px, 100%);
		margin: var(--md-sys-space-sm) auto 0;
		width: 100%;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.awaiting-bg-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--md-sys-color-tertiary, #7c9cff);
		flex-shrink: 0;
		animation: awaiting-bg-pulse 1.2s ease-in-out infinite;
	}
	.awaiting-bg-text {
		line-height: inherit;
	}
	@keyframes awaiting-bg-pulse {
		0%,
		100% {
			opacity: 0.35;
			transform: scale(0.9);
		}
		50% {
			opacity: 1;
			transform: scale(1);
		}
	}
</style>
