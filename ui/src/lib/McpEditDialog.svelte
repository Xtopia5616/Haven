<script>
	import MaterialDialog from '$lib/MaterialDialog.svelte';

	let { server = null, onClose, onSave, existingNames = [] } = $props();

	let isEdit = $derived(server !== null);
	let name = $state('');
	let command = $state('');
	let argsText = $state('');
	let envText = $state('');
	let enabled = $state(true);
	let fieldErrors = $state({ name: '', command: '', env: '' });
	let saving = $state(false);

	$effect(() => {
		name = server?.name || '';
		command = server?.command || '';
		argsText = (server?.args || []).join('\n');
		envText = (server?.env || []).join('\n');
		enabled = server?.enabled ?? true;
		fieldErrors = { name: '', command: '', env: '' };
	});

	function validate() {
		const errors = { name: '', command: '', env: '' };
		const trimmedName = name.trim();
		if (!trimmedName) {
			errors.name = 'Name is required';
		} else if (!/^[A-Za-z0-9_-]{1,128}$/.test(trimmedName)) {
			errors.name = 'Use 1-128 characters: A-Z, a-z, 0-9, - or _';
		} else if (!isEdit && existingNames.includes(trimmedName)) {
			errors.name = 'Name already exists';
		}

		if (!command.trim()) {
			errors.command = 'Command is required';
		}

		const envLines = envText
			.split('\n')
			.map((l) => l.trim())
			.filter(Boolean);
		const badEnv = envLines.findIndex((l) => !l.includes('='));
		if (badEnv >= 0) {
			errors.env = `Line ${badEnv + 1}: expected KEY=VALUE`;
		}

		return errors;
	}

	async function handleSave() {
		const errors = validate();
		fieldErrors = errors;
		if (errors.name || errors.command || errors.env) return;
		saving = true;
		const config = {
			name: name.trim(),
			transport: 'Stdio',
			command: command.trim(),
			args: argsText
				.split('\n')
				.map((l) => l.trim())
				.filter(Boolean),
			env: envText
				.split('\n')
				.map((l) => l.trim())
				.filter(Boolean),
			enabled,
		};
		await onSave(config);
		saving = false;
	}

	function handleOverlayClick(e) {
		if (e.target === e.currentTarget) onClose();
	}

	function handleKeydown(e) {
		if (e.key === 'Enter' && e.target.tagName !== 'TEXTAREA') {
			e.preventDefault();
			handleSave();
		}
	}
</script>

<MaterialDialog open={true} onClose={onClose} title={isEdit ? 'Edit MCP Server' : 'Add MCP Server'}>
	{#snippet footer()}
		<button class="md-btn md-btn--tonal" onclick={onClose}>Cancel</button>
		<button class="md-btn md-btn--filled" onclick={handleSave} disabled={saving}>
			{saving ? 'Saving...' : isEdit ? 'Update' : 'Add'}
		</button>
	{/snippet}
	<div class="dialog-content" onkeydown={handleKeydown} role="presentation">
		<label>
			<span>Name</span>
			<input
				type="text"
				class="md-input"
				bind:value={name}
				placeholder="my-server"
				disabled={isEdit}
				autocomplete="off"
				class:input-error={fieldErrors.name}
			/>
			{#if fieldErrors.name}
				<span class="field-error">{fieldErrors.name}</span>
			{/if}
		</label>
		<div class="field">
			<span>Transport</span>
			<div class="transport-static">Stdio (local process)</div>
		</div>
		<label>
			<span>Command</span>
			<input
				type="text"
				class="md-input"
				bind:value={command}
				placeholder="python"
				autocomplete="off"
				class:input-error={fieldErrors.command}
			/>
			{#if fieldErrors.command}
				<span class="field-error">{fieldErrors.command}</span>
			{/if}
		</label>
		<label>
			<span>Args (one per line)</span>
			<textarea
				class="md-textarea"
				bind:value={argsText}
				rows="3"
				placeholder="-m&#10;mcp_server"
				autocomplete="off"
			></textarea>
		</label>
		<label>
			<span>Env (KEY=VALUE, one per line)</span>
			<textarea
				class="md-textarea"
				bind:value={envText}
				rows="3"
				placeholder="API_KEY=abc123"
				autocomplete="off"
				class:input-error={fieldErrors.env}
			></textarea>
			{#if fieldErrors.env}
				<span class="field-error">{fieldErrors.env}</span>
			{/if}
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
	.dialog-content label,
	.dialog-content .field {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.dialog-content label > span:first-child,
	.dialog-content .field > span:first-child {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}
	.dialog-content input[type="text"],
	.dialog-content textarea {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface);
		font-size: 15px;
		font-family: inherit;
		transition: border-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.dialog-content input:focus,
	.dialog-content textarea:focus {
		outline: none;
		border-color: var(--md-sys-color-primary);
	}
	.dialog-content input.input-error,
	.dialog-content textarea.input-error {
		border-color: var(--md-sys-color-error);
	}
	.dialog-content textarea {
		resize: vertical;
		font-family: var(--md-sys-typescale-mono);
		font-size: 12px;
	}
	.transport-static {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 15px;
	}
	.field-error {
		font-size: 12px;
		color: var(--md-sys-color-error);
	}
	.checkbox-label {
		flex-direction: row !important;
		align-items: center;
		gap: var(--md-sys-space-sm) !important;
	}
	.checkbox-label input[type="checkbox"] {
		accent-color: var(--md-sys-color-primary);
	}
</style>
