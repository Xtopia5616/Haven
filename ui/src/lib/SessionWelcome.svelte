<script lang="ts">
	import { fly } from 'svelte/transition';
	import Logo from './Logo.svelte';

	interface Props {
		hotkeyBinding?: string;
		animated?: boolean;
		showModelSetup?: boolean;
		onSelectPrompt?: (prompt: string) => void;
		onConfigureModel?: () => void;
	}

	let {
		hotkeyBinding = 'Ctrl+Shift+Space',
		animated = false,
		showModelSetup = false,
		onSelectPrompt = () => {},
		onConfigureModel = () => {},
	}: Props = $props();

	const examplePrompts = [
		'请把这段会议记录整理成摘要和待办清单。',
		'帮我检查当前工作区最近修改的文件，并总结改动。',
		'提醒我明天下午 3 点准备周会。',
	];
</script>

<div class="welcome" in:fly={{ y: animated ? 12 : 0, duration: animated ? 330 : 0 }}>
	<div class="welcome-mark"><Logo size={48} /></div>
	<h2>Haven</h2>
	<span class="welcome-kicker">本地 AI 助手</span>
	<p>输入任务，Haven 会在需要时调用本地工具协助完成。</p>
	<div class="welcome-hints" aria-label="可用输入方式">
		<span>直接输入</span>
		<span>{hotkeyBinding} 语音</span>
		<span>图片与文件</span>
	</div>
	<div class="welcome-examples" aria-label="试试这样提问">
		<p>试试这样开始</p>
		<div class="welcome-examples__list">
			{#each examplePrompts as prompt (prompt)}
				<button type="button" onclick={() => onSelectPrompt(prompt)}>{prompt}</button>
			{/each}
		</div>
		<span class="welcome-examples__hint">选择示例只会填入输入框，确认后再发送。</span>
	</div>
	{#if showModelSetup}
		<div class="welcome-setup">
			<span>开始前先配置一个聊天模型。</span>
			<button type="button" onclick={() => onConfigureModel()}>配置模型</button>
		</div>
	{/if}
</div>

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
	.welcome-examples {
		width: min(100%, 560px);
		margin-top: var(--md-sys-space-sm);
		text-align: left;
	}
	.welcome-examples > p {
		margin: 0 0 var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
	}
	.welcome-examples__list {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: var(--md-sys-space-xs);
	}
	.welcome-examples__list button {
		min-height: 56px;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface);
		font: inherit;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		text-align: left;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.welcome-examples__list button:hover {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-surface-container);
	}
	.welcome-examples__list button:focus-visible,
	.welcome-setup button:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: 2px;
	}
	.welcome-examples__hint {
		display: block;
		margin-top: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.welcome-setup {
		display: flex;
		align-items: center;
		justify-content: center;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.welcome-setup button {
		min-height: 36px;
		padding: 0 var(--md-sys-space-md);
		border: 0;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		font: inherit;
		font-weight: 650;
		cursor: pointer;
	}
	@media screen and (max-width: 640px) {
		.welcome-examples__list {
			grid-template-columns: 1fr;
		}
		.welcome-examples__list button {
			min-height: 44px;
		}
	}
</style>
