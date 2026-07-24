<script>
	import MaterialDialog from '$lib/MaterialDialog.svelte';

	let { server = null, onClose, onSave, existingNames = [] } = $props();

	let isEdit = $derived(server !== null);
	let name = $state('');
	let command = $state('');
	let argsText = $state('');
	let envText = $state('');
	let enabled = $state(true);
	let error = $state('');
	let saving = $state(false);

	$effect(() => {
		name = server?.name || '';
		command = server?.command || '';
		argsText = (server?.args || []).join('\n');
		envText = (server?.env || []).join('\n');
		enabled = server?.enabled ?? true;
	});

	function validate() {
		if (!name.trim()) return 'Name is required';
		if (!command.trim()) return 'Command is required';
		if (!isEdit && existingNames.includes(name.trim())) return 'Name already exists';
		return '';
	}

	async function handleSave() {
		const err = validate();
		if (err) {
			error = err;
			return;
		}
		error = '';
		saving = true;
		const config = {
			name: name.trim(),
			transport: 'Stdio',
			command: command.trim(),
			args: argsText.split('\n').filter((a) => a.trim()),
			env: envText.split('\n').filter((a) => a.trim()),
			enabled,
		};
		await onSave(config);
		saving = false;
	}

	function handleOverlayClick(e) {
		if (e.target === e.currentTarget) onClose();
	}
</script>

<MaterialDialog open={true} onClose={onClose} title={isEdit ? 'Edit MCP Server' : 'Add MCP Server'}>
	{#snippet footer()}
		<button class="md-btn md-btn--tonal" onclick={onClose}>Cancel</button>
		<button class="md-btn md-btn--filled" onclick={handleSave} disabled={saving}>
			{saving ? 'Saving...' : isEdit ? 'Update' : 'Add'}
		</button>
	{/snippet}
	<div class="dialog-content">
		{#if error}
			<div class="error">{error}</div>
		{/if}
		<label>
			<span>Name</span>
			<input type="text" class="md-input" bind:value={name} placeholder="my-server" disabled={isEdit} />
		</label>
		<label>
			<span>Transport</span>
			<select class="md-select" disabled>
				<option selected>Stdio</option>
			</select>
		</label>
		<label>
			<span>Command</span>
			<input type="text" class="md-input" bind:value={command} placeholder="python" />
		</label>
		<label>
			<span>Args (one per line)</span>
			<textarea class="md-textarea" bind:value={argsText} rows="3" placeholder="-m&#10;mcp_server"></textarea>
		</label>
		<label>
			<span>Env (KEY=VALUE, one per line)</span>
			<textarea class="md-textarea" bind:value={envText} rows="3" placeholder="API_KEY=abc123"></textarea>
		</label>
		<label class="checkbox-label">
			<input type="checkbox" bind:checked={enabled} />
			<span>Enabled</span>
		</label>
	</div>
</MaterialDialog>

<style>
	.dialog-content {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.dialog-content label {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.dialog-content label span {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}
	.dialog-content input[type="text"],
	.dialog-content textarea,
	.dialog-content select {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface);
		font-size: 15px;
		font-family: inherit;
		transition: border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.dialog-content input:focus,
	.dialog-content textarea:focus {
		outline: none;
		border-color: var(--md-sys-color-primary);
	}
	.dialog-content textarea {
		resize: vertical;
		font-family: var(--md-sys-typescale-mono);
		font-size: 12px;
	}
	.checkbox-label {
		flex-direction: row !important;
		align-items: center;
		gap: var(--md-sys-space-sm) !important;
	}
	.checkbox-label input[type="checkbox"] {
		accent-color: var(--md-sys-color-primary);
	}
	.error {
		background: var(--md-sys-color-error-container);
		border: 1px solid var(--md-sys-color-error);
		color: var(--md-sys-color-on-error-container);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-extra-small);
		font-size: 12px;
	}
</style>