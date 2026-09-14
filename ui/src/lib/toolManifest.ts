/** Canonical frontend view of the backend-owned tool catalog manifest. */

export type ToolSource = 'builtin' | 'skill' | 'mcp' | string;

export type ToolManifest = {
	identity: {
		source: ToolSource;
		catalogGroup: string;
		root: string;
		operation: string | null;
		stableName: string;
	};
	model: { name: string; description: string; inputSchema: unknown };
	policy: {
		riskLevel: string;
		permissionKey: string;
		confirmation: string;
		idempotency: string;
		scope: string;
		concurrency: string;
		effect?: string;
		dataSensitivity?: string;
		networkAccess?: string;
	};
	presentation: {
		label: string;
		renderer: string;
		icon: string;
		representedSource: ToolSource;
	};
	rootPresentation: { label: string; description: string; icon: string };
	prompt: { whenToUse: string; whenNotToUse: string; keyOperations: string[] };
	availability: {
		enabled: boolean;
		available: boolean;
		availabilityReason: string | null;
		requiresConnection: boolean;
		requiresPermission: boolean;
	};
};

let manifests = new Map<string, ToolManifest>();

/** Convert the snake_case Tauri payload into the camelCase UI contract. */
export function parseToolManifest(value: unknown): ToolManifest | null {
	const candidate = unwrapManifest(value);
	if (!candidate || typeof candidate !== 'object') return null;
	const raw = candidate as Record<string, any>;
	const identity = raw.identity || {};
	const stableName = stringOr(identity.stable_name, stringOr(raw.name, ''));
	if (!stableName) return null;
	const root = stringOr(identity.root, stableName.split('.')[0] || stableName);
	const model = raw.model || {};
	const policy = raw.policy || {};
	const presentation = raw.presentation || {};
	const prompt = raw.prompt || {};
	const availability = raw.availability || {};
	const rootPresentation = raw.root_presentation || {};
	const source = stringOr(identity.source, 'builtin');
	return {
		identity: {
			source,
			catalogGroup: stringOr(identity.catalog_group, stringOr(raw.catalog_group, 'other')),
			root,
			operation: typeof identity.operation === 'string' ? identity.operation : null,
			stableName,
		},
		model: {
			name: stringOr(model.name, stableName),
			description: stringOr(model.description, stringOr(raw.description, '')),
			inputSchema: model.input_schema ?? raw.input_schema ?? {},
		},
		policy: {
			riskLevel: stringOr(policy.risk_level, stringOr(raw.risk_level, 'unknown')),
			permissionKey: stringOr(policy.permission_key, ''),
			confirmation: stringOr(policy.confirmation, 'none'),
			idempotency: stringOr(policy.idempotency, 'unknown'),
			scope: stringOr(policy.scope, 'session'),
			concurrency: stringOr(policy.concurrency, 'exclusive'),
			effect: optionalString(policy.effect),
			dataSensitivity: optionalString(policy.data_sensitivity),
			networkAccess: optionalString(policy.network_access),
		},
		presentation: {
			label: stringOr(presentation.label, stringOr(raw.label, stableName)),
			renderer: stringOr(presentation.renderer, root),
			icon: stringOr(presentation.icon, 'tools'),
			representedSource: stringOr(presentation.represented_source, source),
		},
		rootPresentation: {
			label: stringOr(rootPresentation.label, root),
			description: stringOr(rootPresentation.description, ''),
			icon: stringOr(rootPresentation.icon, 'tools'),
		},
		prompt: {
			whenToUse: stringOr(prompt.when_to_use, ''),
			whenNotToUse: stringOr(prompt.when_not_to_use, ''),
			keyOperations: Array.isArray(prompt.key_operations)
				? prompt.key_operations.filter((item: unknown): item is string => typeof item === 'string')
				: [],
		},
		availability: {
			enabled: availability.enabled !== false && raw.enabled !== false,
			available: availability.available !== false,
			availabilityReason:
				typeof availability.availability_reason === 'string'
					? availability.availability_reason
					: null,
			requiresConnection: availability.requires_connection === true,
			requiresPermission: availability.requires_permission === true,
		},
	};
}

function unwrapManifest(value: unknown): unknown {
	if (!value || typeof value !== 'object') return null;
	const raw = value as Record<string, unknown>;
	if (!('manifest' in raw) || !raw.manifest || typeof raw.manifest !== 'object') return value;
	// A few older callers put the stable name and enabled flag beside a
	// partial manifest. Merge those compatibility fields before normalization.
	return { ...raw, ...(raw.manifest as Record<string, unknown>) };
}

function stringOr(value: unknown, fallback: string): string {
	return typeof value === 'string' && value.length > 0 ? value : fallback;
}

function optionalString(value: unknown): string | undefined {
	return typeof value === 'string' ? value : undefined;
}

/** Replace the live catalog snapshot received from the backend. */
export function setToolManifests(entries: unknown): void {
	const next = new Map<string, ToolManifest>();
	if (Array.isArray(entries)) {
		for (const entry of entries) {
			const manifest = parseToolManifest(entry);
			if (manifest) next.set(manifest.identity.stableName, manifest);
		}
	}
	manifests = next;
}

export function getToolManifest(toolName: string): ToolManifest | null {
	return manifests.get(String(toolName || '')) ?? null;
}

export function toolRendererName(toolName: string): string | null {
	return getToolManifest(toolName)?.presentation.renderer ?? null;
}

/** Tool family/group root, with a compatibility fallback for old messages. */
export function toolRootName(toolName: string): string {
	const manifest = getToolManifest(toolName);
	return manifest?.identity.root || legacyRootName(toolName);
}

export function toolIconName(toolName: string): string | null {
	return getToolManifest(toolName)?.presentation.icon ?? null;
}

export function toolLabel(toolName: string): string | null {
	return getToolManifest(toolName)?.presentation.label ?? null;
}

export function toolRepresentedSource(toolName: string): ToolSource | null {
	return getToolManifest(toolName)?.presentation.representedSource ?? null;
}

function legacyRootName(toolName: string): string {
	const name = String(toolName || '');
	if (name === 'load_mcp' || name.startsWith('mcp__')) return 'mcp';
	if (name === 'load_skill' || name.startsWith('skill__')) return 'skills';
	return name.split('.')[0] || name;
}
