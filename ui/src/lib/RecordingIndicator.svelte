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
		const mins = Math.floor(duration / 60);
		const secs = duration % 60;
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
		<div class="mic-badge" class:speaking class:processing aria-hidden="true">
			<svg viewBox="0 0 24 24" fill="currentColor">
				<path
					d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.91-3c-.49 0-.9.36-.98.85C16.52 14.2 14.47 16 12 16s-4.52-1.8-4.93-4.15c-.08-.49-.49-.85-.98-.85-.61 0-1.09.54-1 1.14.49 3 2.89 5.35 5.91 5.78V20c0 .55.45 1 1 1s1-.45 1-1v-2.08c3.02-.43 5.42-2.78 5.91-5.78.1-.6-.39-1.14-1-1.14z"
				/>
			</svg>
			{#if processing}
				<span class="spinner-ring"></span>
			{/if}
		</div>
		<div class="info">
			<span class="state-label">
				{#if processing}
					处理中…转写
				{:else}
					{speaking ? '正在聆听…' : '请说…'}
				{/if}
			</span>
			<span class="timer">{display}</span>
		</div>
		{#if onCancel && isRecording && !processing}
			<span class="hint">Esc 取消</span>
		{/if}
		{#if isRecording && !processing && reason}
			<span class="reason">结束原因: {reason}</span>
		{/if}
	</div>
{/if}

<style>
	.overlay {
		position: fixed;
		top: 64px;
		left: 50%;
		transform: translateX(-50%);
		z-index: var(--md-sys-z-overlay, 998);
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm) var(--md-sys-space-3xl) var(--md-sys-space-sm) var(--md-sys-space-sm);
		background: var(--md-sys-color-surface);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		box-shadow: var(--md-sys-elevation-3);
	}
	.mic-badge {
		position: relative;
		display: flex;
		align-items: center;
		justify-content: center;
		width: 40px;
		height: 40px;
		border-radius: 50%;
		flex-shrink: 0;
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
		animation: mic-breathe 2s var(--md-sys-motion-easing-emphasized) infinite;
		transition: background-color var(--md-sys-motion-duration-medium)
			var(--md-sys-motion-easing-standard);
	}
	.mic-badge svg {
		width: 22px;
		height: 22px;
	}
	.mic-badge.speaking {
		background: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error);
		animation-duration: 1s;
	}
	.mic-badge.processing {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		animation: none;
	}
	.spinner-ring {
		position: absolute;
		inset: -3px;
		border-radius: 50%;
		border: 2px solid transparent;
		border-top-color: var(--md-sys-color-primary);
		animation: spin 1s linear infinite;
	}
	@keyframes mic-breathe {
		0%,
		100% {
			transform: scale(1);
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--md-sys-color-error) 30%, transparent);
		}
		50% {
			transform: scale(1.08);
			box-shadow: 0 0 0 6px transparent;
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
	.info {
		display: flex;
		flex-direction: column;
		gap: 1px;
	}
	.state-label {
		font-size: 13px;
		font-weight: 700;
		color: var(--md-sys-color-on-surface);
	}
	.overlay.processing .state-label {
		color: var(--md-sys-color-primary);
	}
	.timer {
		font-family: var(--md-sys-typescale-mono);
		font-size: 13px;
		font-weight: 700;
		color: var(--md-sys-color-error);
	}
	.overlay.processing .timer {
		color: var(--md-sys-color-primary);
	}
	.hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.reason {
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.6;
	}
</style>
