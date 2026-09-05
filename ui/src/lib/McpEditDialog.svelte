<script>
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialField from '$lib/MaterialField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import { withStringValue } from '$lib/typedCallbacks.js';

	let { server = null, onClose, onSave, existingNames = [] } = $props();

	let isEdit = $derived(server !== null);
	let name = $state('');
	let transport = $state('stdio');
	let command = $state('');
	let cwd = $state('');
	let argsText = $state('');
	let envText = $state('');
	let url = $state('');
	let fieldErrors = $state({ name: '', command: '', url: '', env: '' });
	let saving = $state(false);

	const transportOptions = [
		{ value: 'stdio', label: 'Stdio (local process)' },
		{ value: 'http', label: 'Streamable HTTP' },
	];

	function isHttp() {
		return transport === 'http';
	}

	$effect(() => {
		name = server?.name || '';
		transport = server?.transport || 'stdio';
		command = server?.command || '';
		cwd = server?.cwd || '';
		argsText = (server?.args || []).join('\n');
		envText = (server?.env || []).join('\n');
		url = server?.url || '';
		fieldErrors = { name: '', command: '', url: '', env: '' };
	});

	function validate() {
		const errors = { name: '', command: '', url: '', env: '' };
		const trimmedName = name.trim();
		if (!trimmedName) {
			errors.name = 'Name is required';
		} else if (!/^[A-Za-z0-9_-]{1,128}$/.test(trimmedName)) {
			errors.name = 'Use 1-128 characters: A-Z, a-z, 0-9, - or _';
		} else if (!isEdit && existingNames.includes(trimmedName)) {
			errors.name = 'Name already exists';
		}

		if (isHttp()) {
			if (!url.trim()) {
				errors.url = 'URL is required';
			} else if (!/^https?:\/\/.+/i.test(url.trim())) {
				errors.url = 'URL must start with http:// or https://';
			}
		} else if (!command.trim()) {
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
		if (errors.name || errors.command || errors.url || errors.env) return;
		saving = true;
		const config = {
			name: name.trim(),
			transport,
			command: command.trim(),
			cwd: cwd.trim() || null,
			args: argsText
				.split('\n')
				.map((l) => l.trim())
				.filter(Boolean),
			env: envText
				.split('\n')
				.map((l) => l.trim())
				.filter(Boolean),
			url: url.trim(),
			// No enable toggle in the dialog: new servers are added enabled,
			// edits keep the current state — enable/disable happens from the
			// server list (pill button) after the server is added.
			enabled: isEdit ? (server?.enabled ?? true) : true,
		};
		await onSave(config);
		saving = false;
	}

	/** @param {MouseEvent} e */
	function handleOverlayClick(e) {
		if (e.target === e.currentTarget) onClose();
	}

	/** @param {KeyboardEvent} e */
	function handleKeydown(e) {
		if (e.key === 'Enter' && /** @type {HTMLElement} */ (e.target).tagName !== 'TEXTAREA') {
			e.preventDefault();
			handleSave();
		}
	}

	/** @param {any} v */
	function handleTransportChange(v) {
		transport = v;
	}
</script>

<MaterialDialog open={true} {onClose} title={isEdit ? 'Edit MCP Server' : 'Add MCP Server'}>
	{#snippet footer()}
		<MaterialButton variant="tonal" label="Cancel" onclick={onClose} />
		<MaterialButton
			variant="filled"
			label={saving ? 'Saving...' : isEdit ? 'Update' : 'Add'}
			onclick={handleSave}
			disabled={saving}
		/>
	{/snippet}
	<div class="dialog-content" onkeydown={handleKeydown} role="presentation">
		<MaterialField label="Name" forId="mcp-name" error={fieldErrors.name}>
			{#snippet children()}
				<input
					id="mcp-name"
					type="text"
					class="md-input"
					bind:value={name}
					placeholder="my-server"
					disabled={isEdit}
					autocomplete="off"
					class:input-error={fieldErrors.name}
				/>
			{/snippet}
		</MaterialField>
		<MaterialField label="Transport">
			{#snippet children()}
				<MaterialSelect
					value={transport}
					options={transportOptions}
					onChange={withStringValue(function handleTransportChange(v) {
						transport = v;
					})}
				/>
			{/snippet}
		</MaterialField>

		{#if isHttp()}
			<MaterialField label="URL" forId="mcp-url" error={fieldErrors.url}>
				{#snippet children()}
					<input
						id="mcp-url"
						type="text"
						class="md-input"
						bind:value={url}
						placeholder="http://localhost:3001/mcp"
						autocomplete="off"
						class:input-error={fieldErrors.url}
					/>
				{/snippet}
			</MaterialField>
			<MaterialField
				label="Headers (KEY=VALUE, one per line)"
				forId="mcp-headers"
				error={fieldErrors.env}
			>
				{#snippet children()}
					<textarea
						id="mcp-headers"
						class="md-textarea"
						bind:value={envText}
						rows="3"
						placeholder="AUTHORIZATION=Bearer abc123"
						autocomplete="off"
						class:input-error={fieldErrors.env}></textarea>
				{/snippet}
			</MaterialField>
		{:else}
			<MaterialField label="Command" forId="mcp-command" error={fieldErrors.command}>
				{#snippet children()}
					<input
						id="mcp-command"
						type="text"
						class="md-input"
						bind:value={command}
						placeholder="python"
						autocomplete="off"
						class:input-error={fieldErrors.command}
					/>
				{/snippet}
			</MaterialField>
			<MaterialField label="CWD (optional)" forId="mcp-cwd">
				{#snippet children()}
					<input
						id="mcp-cwd"
						type="text"
						class="md-input"
						bind:value={cwd}
						placeholder="C:\path\to\server"
						autocomplete="off"
					/>
				{/snippet}
			</MaterialField>
			<MaterialField label="Args (one per line)" forId="mcp-args">
				{#snippet children()}
					<textarea
						id="mcp-args"
						class="md-textarea"
						bind:value={argsText}
						rows="3"
						placeholder="-m&#10;mcp_server"
						autocomplete="off"></textarea>
				{/snippet}
			</MaterialField>
			<MaterialField
				label="Env (KEY=VALUE, one per line)"
				forId="mcp-env"
				error={fieldErrors.env}
			>
				{#snippet children()}
					<textarea
						id="mcp-env"
						class="md-textarea"
						bind:value={envText}
						rows="3"
						placeholder="API_KEY=abc123"
						autocomplete="off"
						class:input-error={fieldErrors.env}></textarea>
				{/snippet}
			</MaterialField>
		{/if}
	</div>
</MaterialDialog>

<style>
	.dialog-content {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.dialog-content input[type='text'],
	.dialog-content textarea {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-large-size);
		line-height: var(--md-sys-typescale-body-large-line-height);
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
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
	}
</style>
