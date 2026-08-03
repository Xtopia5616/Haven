<script>
	import { cubicOut } from 'svelte/easing';

	/** @type {{ isRecording?: boolean; processing?: boolean; duration?: number; vadState?: string; reason?: string | null; onCancel?: (() => Promise<void>) | null }} */
	let {
		isRecording = false,
		processing = false,
		duration = 0,
		vadState = 'silent',
		reason = null,
		onCancel = null,
	} = $props();

	// Entrance transition replacing the old CSS dropIn keyframes. The overlay
	// is centered with a static `transform: translateX(-50%)`, so the inline
	// transform must keep that offset during the animation or the overlay
	// would jump off-center.
	function dropIn(node, { duration = 300 } = {}) {
		return {
			duration,
			easing: cubicOut,
			css: (t) => `transform: translate(-50%, ${-10 * (1 - t)}px); opacity: ${t}`,
		};
	}

	const display = $derived.by(() => {
		const base = processing ? duration : duration;
		const mins = Math.floor(base / 60);
		const secs = base % 60;
		return `${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
	});

	// Breathing intensity scales with VAD state: speech active => brighter/larger.
	const speaking = $derived(vadState === 'speech' && isRecording && !processing);

	function handleEsc(e) {
		// Cancel whenever the overlay is visible (recording or processing):
		// cancel_recording is a no-op when the pipeline is idle, so an extra
		// ESC press can't break anything.
		if ((e.key === 'Escape' || e.key === 'Esc') && (isRecording || processing) && onCancel) {
			e.preventDefault();
			onCancel();
		}
	}
</script>

<svelte:window onkeydown={handleEsc} />

	{#if isRecording || processing}
	<div
		class="overlay"
		class:speaking
		class:processing
		role="status"
		aria-live="assertive"
		aria-label={processing ? 'transcribing' : 'recording'}
		in:dropIn
	>
		<div class="pulse-ring"></div>
		<div class="pulse-ring delay"></div>
		<div class="core" class:speaking class:processing></div>
		<div class="info">
			{#if processing}
				<span class="state-label">处理中…转写</span>
			{:else}
				<span class="state-label">{speaking ? '正在聆听…' : '请说…'}</span>
			{/if}
			<span class="timer">{display}</span>
			{#if onCancel}
				<span class="hint">Esc 取消</span>
			{/if}
			{#if isRecording && !processing && reason}
				<span class="reason">结束原因: {reason}</span>
			{/if}
		</div>
	</div>
{/if}

<style>
	.overlay {
		position: fixed;
		top: 64px;
		left: 50%;
		transform: translateX(-50%);
		z-index: 998;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-lg);
		padding: var(--md-sys-space-md) var(--md-sys-space-xl);
		background: color-mix(in srgb, var(--md-sys-color-surface-container-highest) 92%, transparent);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-extra-large);
		box-shadow: var(--md-sys-elevation-3);
		backdrop-filter: blur(8px);
	}
	.core {
		position: relative;
		width: 18px;
		height: 18px;
		border-radius: 50%;
		background: var(--md-sys-color-error);
		animation: breathe 1.1s var(--md-sys-motion-easing-emphasized) infinite;
		box-shadow: 0 0 8px color-mix(in srgb, var(--md-sys-color-error) 60%, transparent);
	}
	.core.speaking {
		animation: breatheSpeak 0.55s var(--md-sys-motion-easing-emphasized) infinite;
		box-shadow: 0 0 14px color-mix(in srgb, var(--md-sys-color-error) 95%, transparent);
	}
	.core.processing {
		background: var(--md-sys-color-primary);
		box-shadow: 0 0 10px color-mix(in srgb, var(--md-sys-color-primary) 80%, transparent);
		animation: spin 1s linear infinite;
		border-radius: 40%;
	}
	.pulse-ring {
		position: absolute;
		left: 11px;
		width: 18px;
		height: 18px;
		border-radius: 50%;
		border: 2px solid color-mix(in srgb, var(--md-sys-color-error) 60%, transparent);
		opacity: 0;
		animation: ripple 1.4s var(--md-sys-motion-easing-standard) infinite;
	}
	.pulse-ring.delay {
		animation-delay: 0.7s;
	}
	.overlay.processing .pulse-ring {
		border-color: color-mix(in srgb, var(--md-sys-color-primary) 60%, transparent);
	}
	@keyframes breathe {
		0%,
		100% {
			transform: scale(1);
			opacity: 1;
		}
		50% {
			transform: scale(0.8);
			opacity: 0.7;
		}
	}
	@keyframes breatheSpeak {
		0%,
		100% {
			transform: scale(1.25);
			opacity: 1;
		}
		50% {
			transform: scale(0.85);
			opacity: 0.85;
		}
	}
	@keyframes spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}
	@keyframes ripple {
		0% {
			transform: scale(1);
			opacity: 0.6;
		}
		100% {
			transform: scale(2.8);
			opacity: 0;
		}
	}
	.info {
		display: flex;
		flex-direction: column;
		gap: 1px;
		font-size: 12px;
	}
	.state-label {
		font-weight: 700;
		color: var(--md-sys-color-on-surface);
	}
	.overlay.processing .state-label {
		color: var(--md-sys-color-primary);
	}
	.timer {
		font-family: var(--md-sys-typescale-mono);
		font-size: 13px;
		color: var(--md-sys-color-error);
		font-weight: 700;
	}
	.overlay.processing .timer {
		color: var(--md-sys-color-primary);
	}
	.hint {
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.reason {
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.6;
	}
</style>
