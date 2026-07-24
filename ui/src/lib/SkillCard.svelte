<script>
	let { skill, onToggle, onSelect } = $props();

	function handleToggle() {
		onToggle?.(skill.name, !skill.enabled);
	}
</script>

<button class="card" onclick={() => onSelect?.(skill)}>
	<div class="card-header">
		<span class="name">{skill.name}</span>
		<div class="badges">
			{#if skill.version}
				<span class="badge version">{skill.version}</span>
			{/if}
			<span class="badge lang">{skill.language}</span>
		</div>
	</div>
	<p class="desc">{skill.description || 'No description'}</p>
	<div class="card-footer">
		<label class="toggle">
			<input type="checkbox" checked={skill.enabled} onchange={handleToggle} />
			<span class="slider"></span>
			<span class="toggle-label">{skill.enabled ? 'Enabled' : 'Disabled'}</span>
		</label>
		{#if skill.has_script}
			<span class="script-badge">script</span>
		{/if}
	</div>
</button>

<style>
	.card {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-lg);
		cursor: pointer;
		text-align: left;
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		width: 100%;
	}
	.card:hover {
		border-color: var(--md-sys-color-primary);
		background: color-mix(in srgb, var(--md-sys-color-primary) 4%, var(--md-sys-color-surface-container-low));
	}
	.card-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
	}
	.name {
		font-size: 15px;
		font-weight: 600;
		color: var(--md-sys-color-primary);
	}
	.badges {
		display: flex;
		gap: var(--md-sys-space-xs);
	}
	.badge {
		font-size: 10px;
		padding: var(--md-sys-space-2xs) var(--md-sys-space-xs);
		border-radius: var(--md-sys-shape-extra-small);
		font-weight: 600;
	}
	.badge.version {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.badge.lang {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.desc {
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-md);
		line-height: 1.4;
	}
	.card-footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.toggle {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		cursor: pointer;
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.toggle input {
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
	.toggle input:checked + .slider {
		background: var(--md-sys-color-primary);
	}
	.toggle input:checked + .slider::after {
		left: 18px;
		background: var(--md-sys-color-on-primary);
	}
	.toggle-label {
		min-width: 52px;
	}
	.script-badge {
		font-size: 10px;
		padding: var(--md-sys-space-2xs) var(--md-sys-space-xs);
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
		border-radius: var(--md-sys-shape-extra-small);
		font-weight: 600;
	}
</style>