<script>
	/**
	 * VoiceBars — shared animated voice-bar visual primitive.
	 *
	 * @prop {'float'|'equalizer'} pattern — loading float or recording equalizer
	 * @prop {number} count — number of bars to render
	 * @prop {'primary'|'error'} tone — semantic bar color
	 * @prop {'idle'|'active'|'processing'} state — equalizer energy state
	 */
	let { pattern = 'float', count = 3, tone = 'primary', state = 'idle' } = $props();
	let indexes = $derived.by(() =>
		Array.from({ length: Math.max(0, Math.floor(count)) }, (_, index) => index),
	);
</script>

<div
	class="voice-bars voice-bars--{pattern}"
	data-tone={tone}
	data-state={state}
	aria-hidden="true"
>
	{#each indexes as index}
		<span class="voice-bars__bar" style={`--voice-bar-index: ${index};`}></span>
	{/each}
</div>

<style>
	.voice-bars {
		display: inline-flex;
		align-items: center;
		flex-shrink: 0;
	}
	.voice-bars[data-tone='primary'] .voice-bars__bar {
		background: var(--md-sys-color-primary);
	}
	.voice-bars[data-tone='error'] .voice-bars__bar {
		background: var(--md-sys-color-error);
	}

	.voice-bars--float {
		gap: 4px;
		height: 32px;
	}
	.voice-bars--float .voice-bars__bar {
		display: block;
		width: 5px;
		border-radius: var(--md-sys-shape-full);
		animation: voice-bars-float 1.2s ease-in-out infinite;
	}
	.voice-bars--float .voice-bars__bar:nth-child(1) {
		height: 14px;
		animation-delay: -0.16s;
	}
	.voice-bars--float .voice-bars__bar:nth-child(2) {
		height: 24px;
		animation-delay: 0s;
	}
	.voice-bars--float .voice-bars__bar:nth-child(3) {
		height: 18px;
		animation-delay: 0.16s;
	}

	.voice-bars--equalizer {
		gap: 3px;
		height: 26px;
	}
	.voice-bars--equalizer .voice-bars__bar {
		width: 3px;
		height: 16px;
		border-radius: 2px;
		opacity: 0.55;
		transform-origin: center;
		animation: voice-bars-bounce 1.8s ease-in-out infinite;
		transition:
			height var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-medium)
				var(--md-sys-motion-easing-standard),
			opacity var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard);
	}
	.voice-bars--equalizer[data-state='active'] .voice-bars__bar {
		height: 26px;
		opacity: 1;
		animation-duration: 0.9s;
	}
	.voice-bars--equalizer[data-state='processing'] .voice-bars__bar {
		height: 14px;
		opacity: 0.5;
	}
	.voice-bars--equalizer .voice-bars__bar:nth-child(1) {
		animation-delay: -0.1s;
	}
	.voice-bars--equalizer .voice-bars__bar:nth-child(2) {
		animation-delay: -0.4s;
	}
	.voice-bars--equalizer .voice-bars__bar:nth-child(3) {
		animation-delay: -0.2s;
	}
	.voice-bars--equalizer .voice-bars__bar:nth-child(4) {
		animation-delay: -0.55s;
	}
	.voice-bars--equalizer .voice-bars__bar:nth-child(5) {
		animation-delay: -0.3s;
	}

	@keyframes voice-bars-float {
		0%,
		100% {
			opacity: 0.5;
			transform: translateY(4px);
		}
		50% {
			opacity: 1;
			transform: translateY(-4px);
		}
	}
	@keyframes voice-bars-bounce {
		0%,
		100% {
			transform: scaleY(0.3);
		}
		50% {
			transform: scaleY(1);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.voice-bars__bar {
			animation: none;
		}
		.voice-bars--float .voice-bars__bar {
			opacity: 0.8;
			transform: none;
		}
	}
</style>
