<script>
	import Logo from './Logo.svelte';

	/**
	 * ConversationEmptyState — lightweight welcome surface for the first paint.
	 * @prop {string} hotkeyBinding — configured voice-recording shortcut
	 */
	let { hotkeyBinding = 'Ctrl+Shift+Space' } = $props();
</script>

<div class="welcome">
	<div class="welcome-mark"><Logo size={48} /></div>
	<h2>Haven</h2>
	<span class="welcome-kicker">本地 AI 助手</span>
	<p>按 {hotkeyBinding} 开始录音，或直接输入指令</p>
</div>

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
</style>
