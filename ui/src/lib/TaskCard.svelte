<script>
	import { statusColor } from '$lib/taskStatus.js';
	let { task, onCancel, onPause, onResume } = $props();
	let expanded = $state(false);

	function durationStr(createdAt, updatedAt, status) {
		const start = new Date(createdAt).getTime();
		const end =
			status === 'running' || status === 'pending' || status === 'paused' || status === 'paused_pending'
				? Date.now()
				: new Date(updatedAt).getTime();
		if (isNaN(start) || isNaN(end)) return '';
		const secs = Math.floor((end - start) / 1000);
		if (secs < 60) return `${secs}s`;
		const mins = Math.floor(secs / 60);
		return `${mins}m ${secs % 60}s`;
	}

	function handleCancel(e) {
		e.stopPropagation();
		onCancel?.(task.id);
	}

	function handlePause(e) {
		e.stopPropagation();
		onPause?.(task.id);
	}

	function handleResume(e) {
		e.stopPropagation();
		onResume?.(task.id);
	}
</script>

<div
	class="task-card"
	class:expanded
	role="button"
	tabindex="0"
	onclick={() => (expanded = !expanded)}
	onkeydown={(e) => {
		if (e.key === 'Enter' || e.key === ' ') {
			e.preventDefault();
			expanded = !expanded;
		}
	}}
>
	<div class="task-summary">
		<span class="task-dot" style="color: {statusColor(task.status)}">&#9679;</span>
		<span class="task-title">{task.summary || task.input || task.title || 'Untitled'}</span>
		<span class="task-badge">{task.status}</span>
		<span class="task-duration"
			>{durationStr(task.created_at, task.updated_at, task.status)}</span
		>
	</div>
	<div class="task-actions">
		{#if task.status === 'running' || task.status === 'pending'}
			<button class="task-btn task-btn-danger" onclick={handleCancel} title="Cancel">&#x2715; Cancel</button>
			<button class="task-btn" onclick={handlePause} title="Pause">&#x23F8; Pause</button>
		{:else if task.status === 'paused'}
			<button class="task-btn task-btn-primary" onclick={handleResume} title="Resume">&#x25B6; Resume</button>
			<button class="task-btn task-btn-danger" onclick={handleCancel} title="Cancel">&#x2715; Cancel</button>
		{/if}
	</div>
	{#if expanded && task.steps}
		<div class="task-steps">
			{#each task.steps as step}
				<div class="step-row" class:step-supplement={step.tool_name === 'supplement'}
					><span
						class="step-dot"
						style="color: {step.status === 'completed'
							? '#44cc44'
							: step.status === 'failed'
								? '#ff4444'
								: '#888'}"
					></span>
					<span class="step-tool">{step.tool_name}</span>
					{#if step.tool_name === 'supplement'}
						<span class="step-input">{step.input}</span>
					{/if}
					<span class="step-status">{step.status}</span>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.task-card {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		cursor: pointer;
		transition: border-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.task-card:hover {
		background: var(--md-sys-color-surface-container);
	}
	.task-card.expanded {
		border-color: var(--md-sys-color-primary);
		box-shadow: var(--md-sys-elevation-1);
	}
	.task-summary {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex-wrap: wrap;
	}
	.task-dot {
		font-size: 10px;
	}
	.task-title {
		font-size: 13px;
		color: var(--md-sys-color-on-surface);
		flex: 1;
		min-width: 120px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.task-badge {
		font-size: 10px;
		font-weight: 700;
		letter-spacing: 0.4px;
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
	}
	.task-duration {
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
		font-family: var(--md-sys-typescale-mono);
	}
	.task-actions {
		display: flex;
		gap: var(--md-sys-space-xs);
		margin-top: var(--md-sys-space-sm);
	}
	.task-btn {
		padding: 4px var(--md-sys-space-sm);
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 11px;
		font-weight: 600;
		cursor: pointer;
		transition: background-color var(--md-sys-motion-duration-fast)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.task-btn:hover {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface);
	}
	.task-btn-danger:hover {
		background: var(--md-sys-color-error-container);
		border-color: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error-container);
	}
	.task-btn-primary:hover {
		background: var(--md-sys-color-primary-container);
		border-color: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary-container);
	}
	.task-steps {
		margin-top: var(--md-sys-space-md);
		padding-top: var(--md-sys-space-sm);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.step-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) 0;
		font-size: 11px;
	}
	.step-dot {
		font-size: 8px;
	}
	.step-tool {
		color: var(--md-sys-color-primary);
		font-family: var(--md-sys-typescale-mono);
		font-weight: 600;
	}
	.step-status {
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
		text-transform: capitalize;
	}
	.step-input {
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		font-family: var(--md-sys-typescale-mono);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 240px;
	}
	.step-supplement .step-tool {
		color: var(--md-sys-color-warning);
	}
</style>
