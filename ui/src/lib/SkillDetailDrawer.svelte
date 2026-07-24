<script>
	let { skill, onClose, onToggle } = $props();

	let previewParams = $state('{}');
	let previewResult = $state(null);
	let running = $state(false);
	let showConfirm = $state(false);
	let pendingConfirmResolve = $state(null);
	const previewPlaceholder = '{"key": "value"}';

	import { invoke } from '$lib/tauri.js';
	import ConfirmationDialog from '$lib/ConfirmationDialog.svelte';

	function handleToggle() {
		onToggle?.(skill.name, !skill.enabled);
	}

	async function runPreview() {
		if (running) return;
		running = true;
		previewResult = null;
		let params;
		try {
			params = JSON.parse(previewParams);
		} catch {
			previewResult = 'Invalid JSON params';
			running = false;
			return;
		}

		try {
			const result = await invoke('execute_skill', {
				name: skill.name,
				params,
				confirmed: false,
			});
			previewResult = JSON.stringify(result, null, 2);
		} catch (err) {
			const msg = typeof err === 'string' ? err : String(err);
			try {
				const parsed = JSON.parse(msg);
				if (parsed.requires_confirmation) {
					showConfirm = true;
					const approved = await new Promise((resolve) => {
						pendingConfirmResolve = resolve;
					});
					showConfirm = false;
					if (!approved) {
						previewResult = 'Execution cancelled';
						running = false;
						return;
					}
					try {
						const result2 = await invoke('execute_skill', {
							name: skill.name,
							params,
							confirmed: true,
						});
						previewResult = JSON.stringify(result2, null, 2);
					} catch (err2) {
						previewResult = `Error: ${err2}`;
					}
				} else {
					previewResult = `Error: ${msg}`;
				}
			} catch {
				previewResult = `Error: ${msg}`;
			}
		}
		running = false;
	}

	function handleConfirm({ approved }) {
		pendingConfirmResolve?.(approved);
	}
</script>

{#if skill}
	<div class="backdrop" onclick={onClose} onkeydown={(e) => e.key === 'Escape' && onClose?.()} role="presentation" tabindex="-1">
		<div class="drawer" role="dialog" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
			<div class="drawer-header">
				<h2>{skill.name}</h2>
				<button class="btn-close" onclick={onClose}>✕</button>
			</div>

			<div class="drawer-body">
				<div class="meta-row">
					{#if skill.version}
						<span class="meta-item">Version: {skill.version}</span>
					{/if}
					<span class="meta-item">Language: {skill.language}</span>
					{#if skill.has_script}
						<span class="meta-item script">Has script</span>
					{/if}
				</div>

				<p class="description">{skill.description || 'No description.'}</p>

				<div class="section">
					<h3>Allowed Tools</h3>
					{#if skill.allowed_tools && skill.allowed_tools.length > 0}
						<div class="tags">
							{#each skill.allowed_tools as tool}
								<span class="tag">{tool}</span>
							{/each}
						</div>
					{:else}
						<p class="empty-note">All tools allowed</p>
					{/if}
				</div>

				<div class="section">
					<h3>Instructions</h3>
					<div class="instructions">{skill.instructions || 'No instructions.'}</div>
				</div>

				<div class="section">
					<h3>Root</h3>
					<code class="path">{skill.root}</code>
				</div>

				{#if skill.has_script}
					<div class="section">
						<h3>Execution Preview</h3>
						<div class="preview-row">
							<textarea
								class="preview-input"
								bind:value={previewParams}
								rows="3"
								placeholder={previewPlaceholder}
							></textarea>
							<button class="btn-preview" onclick={runPreview} disabled={running}>
								{running ? 'Running...' : 'Run'}
							</button>
						</div>
						{#if previewResult}
							<pre class="preview-result">{previewResult}</pre>
{/if}

{#if showConfirm}
	<ConfirmationDialog
		stepId="skill-preview"
		toolName={"skill:" + skill.name}
		taskId="preview"
		riskLevel="medium"
		onConfirm={handleConfirm}
	/>
{/if}
					</div>
				{/if}
			</div>

			<div class="drawer-footer">
				<label class="inline-toggle">
					<input type="checkbox" checked={skill.enabled} onchange={handleToggle} />
					<span class="slider"></span>
					<span>{skill.enabled ? 'Enabled' : 'Disabled'}</span>
				</label>
			</div>
		</div>
	</div>
{/if}

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: color-mix(in srgb, var(--md-sys-color-scrim) 50%, transparent);
		backdrop-filter: blur(4px);
		z-index: var(--md-sys-z-drawer);
		display: flex;
		justify-content: flex-end;
	}
	.drawer {
		width: 420px;
		max-width: 90vw;
		height: 100%;
		background: var(--md-sys-color-surface-container-lowest);
		border-left: 1px solid var(--md-sys-color-outline-variant);
		display: flex;
		flex-direction: column;
		overflow-y: auto;
		box-shadow: var(--md-sys-elevation-4);
	}
	.drawer-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: var(--md-sys-space-xl);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.drawer-header h2 {
		font-size: 18px;
		color: var(--md-sys-color-on-surface);
		margin: 0;
	}
	.btn-close {
		background: none;
		border: none;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 18px;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		width: 32px;
		height: 32px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		transition:
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.btn-close:hover {
		color: var(--md-sys-color-on-surface);
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
	}
	.drawer-body {
		flex: 1;
		padding: var(--md-sys-space-xl);
		overflow-y: auto;
	}
	.meta-row {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-md);
	}
	.meta-item {
		font-size: 11px;
		background: var(--md-sys-color-surface-container-high);
		padding: var(--md-sys-space-2xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-extra-small);
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 600;
	}
	.meta-item.script {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}
	.description {
		font-size: 14px;
		color: var(--md-sys-color-on-surface);
		margin-bottom: var(--md-sys-space-xl);
		line-height: 1.5;
	}
	.section {
		margin-bottom: var(--md-sys-space-xl);
	}
	.section h3 {
		font-size: 12px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: 1px;
		margin-bottom: var(--md-sys-space-sm);
	}
	.tags {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs);
	}
	.tag {
		font-size: 11px;
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		padding: var(--md-sys-space-2xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-extra-small);
		font-weight: 600;
	}
	.empty-note {
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.6;
		font-style: italic;
	}
	.instructions {
		font-size: 13px;
		color: var(--md-sys-color-on-surface);
		line-height: 1.6;
		white-space: pre-wrap;
		background: var(--md-sys-color-surface-container-lowest);
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-extra-small);
		max-height: 300px;
		overflow-y: auto;
		border: 1px solid var(--md-sys-color-outline-variant);
	}
	.path {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		word-break: break-all;
	}
	.empty-note {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.6;
		font-style: italic;
		margin-bottom: var(--md-sys-space-sm);
	}
	.preview-row {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: flex-start;
	}
	.preview-input {
		flex: 1;
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-extra-small);
		color: var(--md-sys-color-on-surface);
		font-size: 12px;
		padding: var(--md-sys-space-sm);
		font-family: var(--md-sys-typescale-mono);
		resize: vertical;
		transition: border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.preview-input:focus {
		outline: none;
		border-color: var(--md-sys-color-primary);
	}
	.preview-input:disabled {
		opacity: 0.5;
	}
	.btn-preview {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		border: none;
		border-radius: var(--md-sys-shape-extra-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-lg);
		font-size: 12px;
		cursor: pointer;
		font-weight: 600;
		transition: background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.btn-preview:hover {
		background: color-mix(in srgb, var(--md-sys-color-on-primary) 8%, var(--md-sys-color-primary));
	}
	.btn-preview:disabled {
		opacity: 0.38;
		cursor: not-allowed;
	}
	.preview-result {
		margin-top: var(--md-sys-space-sm);
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-extra-small);
		padding: var(--md-sys-space-sm);
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
	}
	.drawer-footer {
		padding: var(--md-sys-space-lg) var(--md-sys-space-xl);
		border-top: 1px solid var(--md-sys-color-outline-variant);
		display: flex;
		align-items: center;
	}
	.inline-toggle {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		font-size: 13px;
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
	}
	.inline-toggle input {
		display: none;
	}
	.slider {
		width: 32px;
		height: 16px;
		background: var(--md-sys-color-surface-variant);
		border-radius: var(--md-sys-shape-small);
		position: relative;
		transition: background var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.slider::after {
		content: '';
		position: absolute;
		top: 2px;
		left: 2px;
		width: 12px;
		height: 12px;
		background: var(--md-sys-color-on-surface-variant);
		border-radius: 50%;
		transition: all var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.inline-toggle input:checked + .slider {
		background: var(--md-sys-color-primary);
	}
	.inline-toggle input:checked + .slider::after {
		left: 18px;
		background: var(--md-sys-color-on-primary);
	}
</style>