<script>
	import HavenMark from './HavenMark.svelte';

	/** @type {{ color?: string; animate?: boolean }} */
	let { color = 'success', animate = false } = $props();
	const statusColor = $derived(`var(--md-sys-color-${color})`);
</script>

<span class="status-dot" style="--dot-color: var(--md-sys-color-{color});" class:animate>
	<HavenMark size={16} statusColor={statusColor} />
</span>

<style>
	.status-dot {
		display: inline-block;
		width: 16px;
		height: 16px;
		flex-shrink: 0;
		transition: background-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.status-dot :global(.haven-mark) {
		display: block;
	}
	.status-dot.animate {
		animation: pulse 1.2s var(--md-sys-motion-easing-emphasized) infinite;
	}
	@keyframes pulse {
		0%, 100% { opacity: 1; transform: scale(1); }
		50% { opacity: 0.35; transform: scale(0.85); }
	}
</style>
