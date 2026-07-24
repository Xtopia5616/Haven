<script>
	let { stepId, toolName, taskId, riskLevel, onConfirm } = $props();
	let trustSession = $state(false);

	function handle(approved) {
		onConfirm?.({ stepId, approved, trustSession });
	}

	function handleOverlayKeydown(event) {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			handle(false);
		}
	}
</script>

{#if stepId}
	<div
		class="overlay"
		role="button"
		tabindex="0"
		onclick={() => handle(false)}
		onkeydown={handleOverlayKeydown}
	>
		<!-- 内层 dialog：阻止冒泡 -->
		<div class="dialog" role="presentation" tabindex="-1" onclick={(e) => e.stopPropagation()}>
			<h3>High-Risk Operation</h3>
			<div class="detail">
				<div><strong>Tool:</strong> {toolName}</div>
				<div>
					<strong>Risk:</strong>
					<span class="risk risk-{riskLevel || 'medium'}">{riskLevel || 'medium'}</span>
				</div>
			</div>
			<label class="trust">
				<input type="checkbox" bind:checked={trustSession} />
				Trust this tool for this session
			</label>
			<div class="actions">
				<!-- 内部按钮：使用原生 button 标签，语义更准确 -->
				<button class="btn-deny" onclick={() => handle(false)}>Cancel</button>
				<button class="btn-approve" onclick={() => handle(true)}>Confirm</button>
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
		animation: fadeIn var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	@keyframes fadeIn {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}
	.dialog {
		background: var(--md-sys-color-surface-container-high);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-3xl);
		min-width: 340px;
		box-shadow: var(--md-sys-elevation-3);
		animation: dialogIn var(--md-sys-motion-duration-medium)
			var(--md-sys-motion-easing-emphasized);
	}
	@keyframes dialogIn {
		from {
			opacity: 0;
			transform: scale(0.92);
		}
		to {
			opacity: 1;
			transform: scale(1);
		}
	}
	h3 {
		color: var(--md-sys-color-error);
		font-size: 18px;
		font-weight: 700;
		margin-bottom: var(--md-sys-space-lg);
	}
	.detail {
		margin-bottom: var(--md-sys-space-md);
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.detail strong {
		color: var(--md-sys-color-on-surface);
	}
	.risk {
		font-weight: 700;
		text-transform: capitalize;
	}
	.risk-high,
	.risk-critical {
		color: var(--md-sys-color-error);
	}
	.risk-medium {
		color: var(--md-sys-color-warning);
	}
	.risk-low {
		color: var(--md-sys-color-success);
	}
	.trust {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-lg);
		cursor: pointer;
	}
	.actions {
		display: flex;
		gap: var(--md-sys-space-sm);
		justify-content: flex-end;
	}
	.btn-deny,
	.btn-approve {
		padding: 0 var(--md-sys-space-xl);
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
	}
	.btn-deny {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface-variant);
	}
	.btn-deny:hover {
		box-shadow: var(--md-sys-elevation-1);
	}
	.btn-approve {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
	.btn-approve:hover {
		box-shadow: var(--md-sys-elevation-1);
	}
</style>
