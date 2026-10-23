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
		McpCatalogPresentation,
		McpExposureConfigView,
		McpInvocationAdapters,
		McpServerSnapshotView,
		McpToolAvailability,
		McpToolExposurePolicy,
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
	let defaultAvailability = $state<McpToolAvailability>('enabled');
	let defaultCatalog = $state<McpCatalogPresentation>('deferred');
	let defaultInvocation = $state<McpInvocationAdapters>('native');
	let exposureOverridesText = $state('');
	let fieldErrors = $state({
		name: '',
		command: '',
		url: '',
		env: '',
		exposure: '',
	});
	let saving = $state(false);

	const TRANSPORT_OPTIONS = [
		{ value: 'stdio', label: 'Stdio (local process)' },
		{ value: 'http', label: 'Streamable HTTP' },
	];
	const AVAILABILITY_OPTIONS = [
		{ value: 'enabled', label: 'Enabled — calls are allowed by this MCP policy' },
		{ value: 'disabled', label: 'Disabled — no model-facing adapter may call this tool' },
	];
	const CATALOG_OPTIONS = [
		{ value: 'direct', label: 'Direct — list in discovery and expose without loading' },
		{ value: 'deferred', label: 'On demand — discover, then load before native calls' },
		{ value: 'hidden', label: 'Hidden — omit from the general tool catalog' },
	];
	const INVOCATION_OPTIONS = [
		{ value: 'none', label: 'No invocation adapter' },
		{ value: 'native', label: 'Native tool calls' },
		{ value: 'codemode', label: 'Code Mode calls' },
		{ value: 'native_and_codemode', label: 'Native and Code Mode calls' },
	];
	const AVAILABILITY_VALUES = ['enabled', 'disabled'];
	const CATALOG_VALUES = ['direct', 'deferred', 'hidden'];
	const INVOCATION_VALUES = ['none', 'native', 'codemode', 'native_and_codemode'];

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
		defaultAvailability = server?.exposure.default.availability || 'enabled';
		defaultCatalog = server?.exposure.default.catalog || 'deferred';
		defaultInvocation = server?.exposure.default.invocation || 'native';
		exposureOverridesText = Object.entries(server?.exposure.tools || {})
			.map(
				([pattern, policy]) =>
					`${pattern}=${policy.availability},${policy.catalog},${policy.invocation}`,
			)
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
				const [availability, catalog, invocation, ...extra] = line
					.slice(separator + 1)
					.split(',')
					.map((part) => part.trim());
				if (
					separator < 1 ||
					pattern.length > 256 ||
					/[\u0000-\u001f\u007f]/u.test(pattern) ||
					extra.length > 0 ||
					!AVAILABILITY_VALUES.includes(availability) ||
					!CATALOG_VALUES.includes(catalog) ||
					!INVOCATION_VALUES.includes(invocation)
				) {
					errors.exposure = `Line ${index + 1}: use pattern=enabled|disabled,direct|deferred|hidden,none|native|codemode|native_and_codemode`;
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

	function parseExposureOverrides(): Record<string, McpToolExposurePolicy> {
		return Object.fromEntries(
			exposureOverridesText
				.split('\n')
				.map((line) => line.trim())
				.filter(Boolean)
				.map((line) => {
					const separator = line.lastIndexOf('=');
					const [availability, catalog, invocation] = line
						.slice(separator + 1)
						.split(',')
						.map((part) => part.trim());
					return [
						line.slice(0, separator).trim(),
						{
							availability: availability as McpToolAvailability,
							catalog: catalog as McpCatalogPresentation,
							invocation: invocation as McpInvocationAdapters,
						},
					];
				}),
		);
	}

	async function handleSave() {
		const errors = validate();
		fieldErrors = errors;
		if (
			errors.name ||
			errors.command ||
			errors.url ||
			errors.env ||
			errors.exposure
		)
			return;
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
			url: isHttp() ? url.trim() : '',
			exposure: {
				default: {
					availability: defaultAvailability,
					catalog: defaultCatalog,
					invocation: defaultInvocation,
				},
				tools: parseExposureOverrides(),
			},
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
		if (value === 'stdio' || value === 'http') {
			transport = value;
		}
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
					onChange={withStringValue(handleTransportChange)}
				/>
			{/snippet}
		</MaterialField>
		<MaterialField
			label="Tool availability"
			hint="Disabled tools cannot be called through either adapter. Availability does not replace per-call authorization."
		>
			{#snippet children()}
				<MaterialSelect
					value={defaultAvailability}
					options={AVAILABILITY_OPTIONS}
					onChange={withStringValue((value) => {
						if (AVAILABILITY_VALUES.includes(value))
							defaultAvailability = value as McpToolAvailability;
					})}
				/>
			{/snippet}
		</MaterialField>
		<MaterialField
			label="Catalog presentation"
			hint="Direct lists the tool and sends its schema with requests when a native adapter is enabled. On demand lists it for discovery and requires load_mcp before native calls. Hidden omits it from the general catalog; enabled native or Code Mode adapters still work. Set availability to Disabled to block calls."
		>
			{#snippet children()}
				<MaterialSelect
					value={defaultCatalog}
					options={CATALOG_OPTIONS}
					onChange={withStringValue((value) => {
						if (CATALOG_VALUES.includes(value))
							defaultCatalog = value as McpCatalogPresentation;
					})}
				/>
			{/snippet}
		</MaterialField>
		<MaterialField
			label="Invocation adapters"
			hint="Code Mode composes child calls in a bounded script. Each native or Code Mode call still follows the ordinary authorization and journal path."
		>
			{#snippet children()}
				<MaterialSelect
					value={defaultInvocation}
					options={INVOCATION_OPTIONS}
					onChange={withStringValue((value) => {
						if (INVOCATION_VALUES.includes(value))
							defaultInvocation = value as McpInvocationAdapters;
					})}
				/>
			{/snippet}
		</MaterialField>
		<MaterialField
			label="Per-tool overrides"
			forId="mcp-exposure-overrides"
			hint="One rule per line: pattern=availability,catalog,adapters. Example: calendar_delete=disabled,hidden,none or calendar_*=enabled,deferred,native_and_codemode. Most specific pattern wins."
			error={fieldErrors.exposure}
		>
			{#snippet children()}
				<textarea
					id="mcp-exposure-overrides"
					class="md-textarea md-textarea--code"
					bind:value={exposureOverridesText}
					rows="4"
					placeholder="calendar_delete=disabled,hidden,none&#10;calendar_*=enabled,deferred,native"
					class:input-error={fieldErrors.exposure}></textarea>
			{/snippet}
		</MaterialField>

		{#if isHttp()}
			<MaterialField
				label="URL"
				forId="mcp-url"
				hint="Haven requires the current stateless MCP server/discover handshake. Older initialize/session peers are not supported. Streamable HTTP may return JSON or request-scoped SSE."
				error={fieldErrors.url}
			>
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
