<script>
	/** @type {{ color?: string; animate?: boolean }} */
	let { color = 'success', animate = false } = $props();
</script>

<span class="status-dot" style="--dot-color: var(--md-sys-color-{color});" class:animate></span>

<style>
	.status-dot {
		display: inline-block;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--dot-color, var(--md-sys-color-success));
		flex-shrink: 0;
		transition: background-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.status-dot.animate {
		animation: pulse 1.2s var(--md-sys-motion-easing-emphasized) infinite;
	}
	@keyframes pulse {
		0%, 100% { opacity: 1; transform: scale(1); }
		50% { opacity: 0.35; transform: scale(0.85); }
	}
</style>