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
	const raw = record(value);
	if (!raw || 'manifest' in raw) return null;
	const identity = record(raw.identity);
	const model = record(raw.model);
	const policy = record(raw.policy);
	const presentation = record(raw.presentation);
	const prompt = record(raw.prompt);
	const availability = record(raw.availability);
	const rootPresentation = record(raw.root_presentation);
	if (
		!identity ||
		!model ||
		!policy ||
		!presentation ||
		!prompt ||
		!availability ||
		!rootPresentation
	) {
		return null;
	}
	const source = requiredString(identity.source);
	const catalogGroup = requiredString(identity.catalog_group);
	const root = requiredString(identity.root);
	const stableName = requiredString(identity.stable_name);
	const modelName = requiredString(model.name);
	const modelDescription = requiredString(model.description);
	if (!source || !catalogGroup || !root || !stableName || !modelName || !modelDescription)
		return null;
	if (!('input_schema' in model)) return null;
	const operation = identity.operation;
	if (operation !== null && typeof operation !== 'string') return null;
	const riskLevel = requiredString(policy.risk_level);
	const permissionKey = requiredString(policy.permission_key);
	const confirmation = requiredString(policy.confirmation);
	const idempotency = requiredString(policy.idempotency);
	const scope = requiredString(policy.scope);
	const concurrency = requiredString(policy.concurrency);
	const effect = requiredString(policy.effect);
	const dataSensitivity = requiredString(policy.data_sensitivity);
	const networkAccess = requiredString(policy.network_access);
	const presentationLabel = requiredString(presentation.label);
	const renderer = requiredString(presentation.renderer);
	const icon = requiredString(presentation.icon);
	const representedSource = requiredString(presentation.represented_source);
	const whenToUse = requiredString(prompt.when_to_use);
	const whenNotToUse = requiredString(prompt.when_not_to_use);
	const keyOperations = prompt.key_operations;
	if (
		!riskLevel ||
		!permissionKey ||
		!confirmation ||
		!idempotency ||
		!scope ||
		!concurrency ||
		!effect ||
		!dataSensitivity ||
		!networkAccess ||
		!presentationLabel ||
		!renderer ||
		!icon ||
		!representedSource ||
		!whenToUse ||
		!whenNotToUse ||
		!Array.isArray(keyOperations) ||
		!keyOperations.every((item) => typeof item === 'string')
	) {
		return null;
	}
	const enabled = booleanValue(availability.enabled);
	const available = booleanValue(availability.available);
	const requiresConnection = booleanValue(availability.requires_connection);
	const requiresPermission = booleanValue(availability.requires_permission);
	if (
		enabled === null ||
		available === null ||
		requiresConnection === null ||
		requiresPermission === null
	) {
		return null;
	}
	const availabilityReason = availability.availability_reason;
	if (
		availabilityReason !== undefined &&
		availabilityReason !== null &&
		typeof availabilityReason !== 'string'
	) {
		return null;
	}
	const rootLabel = requiredString(rootPresentation.label);
	const rootDescription = requiredString(rootPresentation.description);
	const rootIcon = requiredString(rootPresentation.icon);
	if (!rootLabel || !rootDescription || !rootIcon) return null;
	return {
		identity: {
			source,
			catalogGroup,
			root,
			operation,
			stableName,
		},
		model: {
			name: modelName,
			description: modelDescription,
			inputSchema: model.input_schema,
		},
		policy: {
			riskLevel,
			permissionKey,
			confirmation,
			idempotency,
			scope,
			concurrency,
			effect,
			dataSensitivity,
			networkAccess,
		},
		presentation: {
			label: presentationLabel,
			renderer,
			icon,
			representedSource,
		},
		rootPresentation: {
			label: rootLabel,
			description: rootDescription,
			icon: rootIcon,
		},
		prompt: {
			whenToUse,
			whenNotToUse,
			keyOperations,
		},
		availability: {
			enabled,
			available,
			availabilityReason: availabilityReason ?? null,
			requiresConnection,
			requiresPermission,
		},
	};
}

function record(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function requiredString(value: unknown): string | null {
	return typeof value === 'string' && value.length > 0 ? value : null;
}

function booleanValue(value: unknown): boolean | null {
	return typeof value === 'boolean' ? value : null;
}

/** Replace the live catalog snapshot received from the backend. */
export function setToolManifests(entries: unknown): ToolManifest[] {
	const next = new Map<string, ToolManifest>();
	const parsedEntries: ToolManifest[] = [];
	if (Array.isArray(entries)) {
		for (const entry of entries) {
			const manifest = parseToolManifest(entry);
			if (manifest) {
				next.set(manifest.identity.stableName, manifest);
				parsedEntries.push(manifest);
			}
		}
	}
	manifests = next;
	return parsedEntries;
}

export function getToolManifest(toolName: string): ToolManifest | null {
	return manifests.get(String(toolName || '')) ?? null;
}

export function toolRendererName(toolName: string): string | null {
	return getToolManifest(toolName)?.presentation.renderer ?? null;
}

/** Tool family/group root from the current backend-owned manifest. */
export function toolRootName(toolName: string): string {
	return getToolManifest(toolName)?.identity.root ?? toolName.split('.')[0];
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
