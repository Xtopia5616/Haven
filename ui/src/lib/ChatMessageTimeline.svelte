<script>
	import { fly } from 'svelte/transition';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import Logo from '$lib/Logo.svelte';

	let {
		messages = [],
		hotkeyBinding = 'Ctrl+Shift+Space',
		awaitingBackground = false,
		awaitingBackgroundCount = 0,
		activeSessionError = false,
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
		<p>按 {hotkeyBinding} 开始录音，或直接输入指令</p>
	</div>
{:else}
	<div class="message-list">
		{#each messages as msg (msg.id)}
			<ChatBubble
				role={msg.role}
				content={msg.content}
				type={msg.type}
				voice={msg.voice}
				time={msg.time}
				streaming={!!msg.streaming}
				toolName={msg.toolName ?? ''}
				messageId={msg.id}
				stepNumber={msg.stepNumber}
				usage={msg.type === 'tool' ? stepUsage(msg.stepNumber) : null}
				toolArgs={msg.toolArgs ?? null}
				attachments={msg.attachments}
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
{/if}

{#if awaitingBackground && !activeSessionError}
	<div class="awaiting-bg-banner" in:fly={{ y: 8, duration: 300 }} role="status">
		<span class="awaiting-bg-dot" aria-hidden="true"></span>
		<span class="awaiting-bg-text">
			等待后台任务结果{#if awaitingBackgroundCount > 1}（{awaitingBackgroundCount}）{/if}，完成后将自动继续
		</span>
	</div>
{/if}

{#if activeSessionError}
	<div class="continue-banner" in:fly={{ y: 8, duration: 300 }}>
		<button
			class="md-btn md-btn--filled continue-btn"
			onclick={() => onContinue()}
			type="button"
		>
			<svg
				width="16"
				height="16"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"><polygon points="5 3 19 12 5 21 5 3" /></svg
			>
			继续生成
		</button>
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
		width: 72px;
		height: 72px;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-extra-large);
		background: var(--md-sys-color-primary-container);
		box-shadow: var(--md-sys-elevation-1);
	}
	.welcome h2 {
		font-family: var(--md-ref-typeface-brand);
		font-size: 30px;
		font-weight: 700;
		letter-spacing: 0.5px;
		color: var(--md-sys-color-primary);
	}
	.welcome-kicker {
		margin-top: calc(-1 * var(--md-sys-space-sm));
		font-size: 12px;
		font-weight: 700;
		letter-spacing: 0.8px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.welcome p {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-size, 14px);
		max-width: 420px;
	}
	.message-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}

	.continue-banner {
		display: flex;
		align-items: center;
		justify-content: flex-start;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		max-width: clamp(600px, 92vw, 800px);
		margin: 0 auto;
		width: 100%;
	}
	.continue-btn {
		gap: var(--md-sys-space-xs);
		font-size: 13px;
	}

	.awaiting-bg-banner {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		max-width: clamp(600px, 92vw, 800px);
		margin: var(--md-sys-space-sm) auto 0;
		width: 100%;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
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
		line-height: 1.4;
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
