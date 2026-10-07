/** Canonical frontend view of the backend-owned tool catalog manifest. */

import {
	RISK_LEVEL_VALUES,
	TOOL_CATALOG_GROUP_VALUES,
	TOOL_SOURCE_VALUES,
	type RiskLevel,
	type ToolCatalogGroup,
	type ToolSource,
} from './contracts/generatedCommands.ts';
import { isRecord } from './contracts/objectGuards.ts';

export type ToolManifestView = {
	identity: {
		source: ToolSource;
		catalogGroup: ToolCatalogGroup;
		root: string;
		operation: string | null;
		stableName: string;
	};
	model: { name: string; description: string; inputSchema: unknown };
	policy: {
		riskLevel: RiskLevel;
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

let manifests = new Map<string, ToolManifestView>();

/** Convert the snake_case Tauri payload into the camelCase UI contract. */
export function parseToolManifest(value: unknown): ToolManifestView | null {
	const raw = isRecord(value) ? value : null;
	if (!raw || 'manifest' in raw) return null;
	const identity = isRecord(raw.identity) ? raw.identity : null;
	const model = isRecord(raw.model) ? raw.model : null;
	const policy = isRecord(raw.policy) ? raw.policy : null;
	const presentation = isRecord(raw.presentation) ? raw.presentation : null;
	const prompt = isRecord(raw.prompt) ? raw.prompt : null;
	const availability = isRecord(raw.availability) ? raw.availability : null;
	const rootPresentation = isRecord(raw.root_presentation) ? raw.root_presentation : null;
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
	const source = generatedEnumValue(TOOL_SOURCE_VALUES, identity.source);
	const catalogGroup = generatedEnumValue(TOOL_CATALOG_GROUP_VALUES, identity.catalog_group);
	const root = requiredString(identity.root);
	const stableName = requiredString(identity.stable_name);
	const modelName = requiredString(model.name);
	const modelDescription = requiredString(model.description);
	if (
		source === null ||
		catalogGroup === null ||
		!root ||
		!stableName ||
		!modelName ||
		!modelDescription
	)
		return null;
	if (!('input_schema' in model)) return null;
	const operation = identity.operation;
	if (operation !== null && typeof operation !== 'string') return null;
	const riskLevel = generatedEnumValue(RISK_LEVEL_VALUES, policy.risk_level);
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
	const representedSource = generatedEnumValue(
		TOOL_SOURCE_VALUES,
		presentation.represented_source,
	);
	const whenToUse = requiredString(prompt.when_to_use);
	const whenNotToUse = requiredString(prompt.when_not_to_use);
	const keyOperations = prompt.key_operations;
	if (
		riskLevel === null ||
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
		representedSource === null ||
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

function requiredString(value: unknown): string | null {
	return typeof value === 'string' && value.length > 0 ? value : null;
}

function generatedEnumValue<const Values extends readonly string[]>(
	values: Values,
	value: unknown,
): Values[number] | null {
	return (values as readonly unknown[]).includes(value) ? (value as Values[number]) : null;
}

function booleanValue(value: unknown): boolean | null {
	return typeof value === 'boolean' ? value : null;
}

/** Replace the live catalog snapshot received from the backend. */
export function setToolManifests(entries: unknown): ToolManifestView[] {
	const next = new Map<string, ToolManifestView>();
	const parsedEntries: ToolManifestView[] = [];
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

export function getToolManifest(toolName: string): ToolManifestView | null {
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
