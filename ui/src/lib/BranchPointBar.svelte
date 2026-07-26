<script>
	let { stepNumber, onRollback, onFork } = $props();
</script>

<div class="branch-bar">
	<span class="branch-dot"></span>
	<span class="branch-label">Step {stepNumber}</span>
	<button class="md-btn md-btn--xs md-btn--tonal" onclick={() => onRollback(stepNumber)}>
		<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
			<polyline points="1 4 1 10 7 10" />
			<path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10" />
		</svg>
		回退到此处
	</button>
	<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => onFork(stepNumber)} title="从此处分叉">
		<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
			<line x1="6" y1="3" x2="6" y2="15" />
			<circle cx="18" cy="6" r="3" />
			<circle cx="6" cy="21" r="3" />
			<line x1="18" y1="9" x2="18" y2="21" />
		</svg>
	</button>
</div>

<style>
	.branch-bar {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) 0;
		opacity: 0.6;
		transition: opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.branch-bar:hover {
		opacity: 1;
	}
	.branch-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--md-sys-color-primary);
		border: 2px solid var(--md-sys-color-primary-container);
		flex-shrink: 0;
	}
	.branch-label {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 500;
		min-width: 52px;
	}
	.branch-bar :global(.md-btn) {
		font-size: 11px;
		height: 26px;
		padding: 0 var(--md-sys-space-sm);
	}
</style>
