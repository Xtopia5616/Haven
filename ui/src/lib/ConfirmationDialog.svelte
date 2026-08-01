<script>
	import { fade, scale } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';

	let { stepId, toolName, taskId, riskLevel, onConfirm } = $props();

	function handleCancel() {
		onConfirm?.({ stepId, approved: false, trustSession: false });
	}

	function handleOnce() {
		onConfirm?.({ stepId, approved: true, trustSession: false });
	}

	function handleSession() {
		onConfirm?.({ stepId, approved: true, trustSession: true });
	}

	function handleOverlayKeydown(event) {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			handleCancel();
		}
	}
</script>

{#if stepId}
	<div
		class="overlay"
		role="button"
		tabindex="0"
		onclick={handleCancel}
		onkeydown={handleOverlayKeydown}
		in:fade={{ duration: 200, easing: cubicOut }}
	>
		<div class="dialog" role="presentation" tabindex="-1" onclick={(e) => e.stopPropagation()} in:scale={{ start: 0.92, duration: 300, easing: cubicOut }}>
			<h3>High-Risk Operation</h3>
			<div class="detail">
				<div><strong>Tool:</strong> {toolName}</div>
				<div>
					<strong>Risk:</strong>
					<span class="risk risk-{riskLevel || 'medium'}">{riskLevel || 'medium'}</span>
				</div>
			</div>
			<div class="actions">
				<button class="btn-deny" onclick={handleCancel}>Cancel</button>
				<div class="btn-group">
					<button class="btn-once" onclick={handleOnce}>仅本次同意</button>
					<button class="btn-session" onclick={handleSession}>本对话同意</button>
				</div>
			</div>
		</div>
	</div>
{/if}

<style>
	.overlay {
		position: fixed;
		inset: 0;
		background: color-mix(in srgb, var(--md-sys-color-scrim) 55%, transparent);
		backdrop-filter: blur(4px);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: var(--md-sys-z-dialog);
	}
	.dialog {
		background: var(--md-sys-color-surface-container-high);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-3xl);
		min-width: 340px;
		box-shadow: var(--md-sys-elevation-3);
	}
	h3 {
		color: var(--md-sys-color-error);
		font-size: 18px;
		font-weight: 700;
		margin-bottom: var(--md-sys-space-lg);
	}
	.detail {
		margin-bottom: var(--md-sys-space-lg);
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.detail strong { color: var(--md-sys-color-on-surface); }
	.risk { font-weight: 700; text-transform: capitalize; }
	.risk-high, .risk-critical { color: var(--md-sys-color-error); }
	.risk-medium { color: var(--md-sys-color-warning); }
	.risk-low { color: var(--md-sys-color-success); }
	.actions {
		display: flex;
		gap: var(--md-sys-space-sm);
		justify-content: flex-end;
		align-items: center;
	}
	.btn-group {
		display: flex;
		gap: var(--md-sys-space-sm);
	}
	.btn-deny, .btn-once, .btn-session {
		padding: 0 var(--md-sys-space-lg);
		height: 40px;
		border: none;
		border-radius: var(--md-sys-shape-small);
		font-family: inherit;
		font-size: 13px;
		font-weight: 700;
		cursor: pointer;
		transition: background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		white-space: nowrap;
	}
	.btn-deny {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface-variant);
	}
	.btn-deny:hover { box-shadow: var(--md-sys-elevation-1); }
	.btn-once {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-primary);
	}
	.btn-once:hover { box-shadow: var(--md-sys-elevation-1); }
	.btn-session {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
	.btn-session:hover { box-shadow: var(--md-sys-elevation-1); }
</style>
