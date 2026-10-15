<script lang="ts">
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialField from '$lib/MaterialField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import { isValidMcpServerName } from '$lib/mcpServerName.ts';
	import { withStringValue } from '$lib/typedCallbacks.ts';
	import type { McpServerTransportInput } from '$lib/contracts/generatedCommands.ts';
	import type { McpServerConfigInput } from '$lib/contracts/tools.ts';
import type {
	McpExposureConfigView,
	McpServerSnapshotView,
	McpToolExposureMode,
} from '$lib/contracts/mcp.ts';

	interface EditorConfig extends McpServerConfigInput {
		name: string;
		transport: McpServerTransportInput;
		command: string;
		cwd: string | null;
		args: string[];
		env: string[];
		url: string;
		enabled: boolean;
	exposure: McpExposureConfigView;
	}

	interface Props {
		server?: McpServerSnapshotView | null;
		onClose: () => void;
		onSave: (config: EditorConfig) => Promise<void> | void;
		existingNames?: string[];
	}

	let { server = null, onClose, onSave, existingNames = [] }: Props = $props();

	let isEdit = $derived(server !== null);
	let name = $state('');
	let transport = $state<McpServerTransportInput>('stdio');
	let command = $state('');
	let cwd = $state('');
	let argsText = $state('');
	let envText = $state('');
	let url = $state('');
	let defaultExposure = $state<McpToolExposureMode>('deferred');
	let exposureOverridesText = $state('');
	let fieldErrors = $state({ name: '', command: '', url: '', env: '', exposure: '' });
	let saving = $state(false);

	const TRANSPORT_OPTIONS = [
		{ value: 'stdio', label: 'Stdio (local process)' },
		{ value: 'http', label: 'Streamable HTTP' },
	];
	const EXPOSURE_OPTIONS = [
		{ value: 'direct', label: 'Legacy · direct — send schemas with every request' },
		{ value: 'model_only', label: 'Model-only — direct model calls only; no nested Workflow or CodeMode' },
		{ value: 'deferred', label: 'Legacy · deferred — discover and load when needed' },
		{ value: 'codemode', label: 'CodeMode — compose allowed tools in a bounded script' },
		{ value: 'hidden', label: 'Hidden — unavailable to the assistant' },
	];

	function isHttp() {
		return transport === 'http';
	}

	$effect(() => {
		name = server?.name || '';
		transport = server?.transport === 'http' ? 'http' : 'stdio';
		command = server?.command || '';
		cwd = server?.cwd || '';
		argsText = (server?.args || []).join('\n');
		envText = (server?.env || []).join('\n');
		url = server?.url || '';
		defaultExposure = server?.exposure.default || 'deferred';
		exposureOverridesText = Object.entries(server?.exposure.tools || {})
			.map(([pattern, mode]) => `${pattern}=${mode}`)
			.join('\n');
		fieldErrors = { name: '', command: '', url: '', env: '', exposure: '' };
	});

	function validate() {
		const errors = { name: '', command: '', url: '', env: '', exposure: '' };
		const trimmedName = name.trim();
		if (!trimmedName) {
			errors.name = 'Name is required';
		} else if (!isValidMcpServerName(trimmedName)) {
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
		const badEnv = envLines.findIndex((l) => {
			const separator = l.indexOf('=');
			return separator === 0;
		});
		if (badEnv >= 0) {
			errors.env = `Line ${badEnv + 1}: environment variable name cannot be empty`;
		}

		const exposureLines = exposureOverridesText
			.split('\n')
			.map((line) => line.trim())
			.filter(Boolean);
		if (exposureLines.length > 256) {
			errors.exposure = 'At most 256 per-tool overrides are allowed';
		} else {
			const patterns = new Set<string>();
			for (const [index, line] of exposureLines.entries()) {
				const separator = line.lastIndexOf('=');
				const pattern = line.slice(0, separator).trim();
				const mode = line.slice(separator + 1).trim();
				if (
					separator < 1 ||
					pattern.length > 256 ||
					/[\u0000-\u001f\u007f]/u.test(pattern) ||
					!['direct', 'model_only', 'deferred', 'codemode', 'hidden'].includes(mode)
				) {
					errors.exposure = `Line ${index + 1}: use pattern=direct, pattern=model_only, pattern=deferred, pattern=codemode, or pattern=hidden`;
					break;
				}
				if (patterns.has(pattern)) {
					errors.exposure = `Line ${index + 1}: duplicate tool pattern`;
					break;
				}
				patterns.add(pattern);
			}
		}

		return errors;
	}

	function parseExposureOverrides(): Record<string, McpToolExposureMode> {
		return Object.fromEntries(
			exposureOverridesText
				.split('\n')
				.map((line) => line.trim())
				.filter(Boolean)
				.map((line) => {
					const separator = line.lastIndexOf('=');
					return [
						line.slice(0, separator).trim(),
						line.slice(separator + 1).trim() as McpToolExposureMode,
					];
				}),
		);
	}

	async function handleSave() {
		const errors = validate();
		fieldErrors = errors;
		if (errors.name || errors.command || errors.url || errors.env || errors.exposure) return;
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
			exposure: { default: defaultExposure, tools: parseExposureOverrides() },
			// No enable toggle in the dialog: new servers are added enabled,
			// edits keep the current state — enable/disable happens from the
			// server list (pill button) after the server is added.
			enabled: isEdit ? (server?.enabled ?? true) : true,
		};
		await onSave(config);
		saving = false;
	}

	function handleOverlayClick(e: MouseEvent) {
		if (e.target === e.currentTarget) onClose();
	}

	function handleKeydown(e: KeyboardEvent) {
		if (
			e.key === 'Enter' &&
			e.target instanceof HTMLElement &&
			e.target.tagName !== 'TEXTAREA'
		) {
			e.preventDefault();
			handleSave();
		}
	}

	function handleTransportChange(value: string) {
		if (value === 'stdio' || value === 'http') transport = value;
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
					options={TRANSPORT_OPTIONS}
					onChange={withStringValue(function handleTransportChange(v) {
						transport = v === 'http' ? 'http' : 'stdio';
					})}
				/>
			{/snippet}
		</MaterialField>
		<MaterialField
			label="Assistant exposure"
			hint="Legacy uses ordinary tool calls. Model-only tools are callable only as top-level model calls, never as nested Workflow or CodeMode calls. CodeMode composes only listed tools in a bounded script; each call is authorized separately. Calls requiring confirmation must use Legacy. Hidden tools cannot be called."
		>
			{#snippet children()}
				<MaterialSelect
					value={defaultExposure}
					options={EXPOSURE_OPTIONS}
					onChange={withStringValue((value) => {
						if (['direct', 'model_only', 'deferred', 'codemode', 'hidden'].includes(value)) {
							defaultExposure = value as McpToolExposureMode;
						}
					})}
				/>
			{/snippet}
		</MaterialField>
		<MaterialField
			label="Per-tool overrides"
			forId="mcp-exposure-overrides"
			hint="One rule per line: exact MCP tool name or * pattern = direct, model_only, deferred, codemode, or hidden. Most specific pattern wins."
			error={fieldErrors.exposure}
		>
			{#snippet children()}
				<textarea
					id="mcp-exposure-overrides"
					class="md-textarea md-textarea--code"
					bind:value={exposureOverridesText}
					rows="4"
					placeholder="calendar_delete=hidden&#10;calendar_*=direct"
					class:input-error={fieldErrors.exposure}
				></textarea>
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
						class="md-textarea md-textarea--code"
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
						class="md-textarea md-textarea--code"
						bind:value={argsText}
						rows="3"
						placeholder="-m&#10;mcp_server"
						autocomplete="off"></textarea>
				{/snippet}
			</MaterialField>
			<MaterialField
				label="Env (KEY=VALUE or KEY, one per line)"
				forId="mcp-env"
				hint="Existing values appear as KEY=&lt;redacted&gt;; keep the marker to preserve the saved value. KEY leaves the value unset."
				error={fieldErrors.env}
			>
				{#snippet children()}
					<textarea
						id="mcp-env"
						class="md-textarea md-textarea--code"
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
	.dialog-content .input-error,
	.dialog-content .input-error:focus {
		border-color: var(--md-sys-color-error);
	}
</style>
