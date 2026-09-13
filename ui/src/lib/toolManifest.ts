/** Runtime view of the backend-owned tool catalog manifest. */

export type ToolManifest = {
	identity: {
		source: 'builtin' | 'skill' | 'mcp' | string;
		catalog_group: string;
		root: string;
		operation?: string | null;
		stable_name: string;
	};
	model: { name: string; description: string; input_schema: unknown };
	policy: {
		risk_level: string;
		permission_key: string;
		confirmation: string;
		idempotency: string;
		scope: string;
		concurrency: string;
	};
	presentation: { label: string; renderer: string; icon: string };
	prompt: { when_to_use: string; when_not_to_use: string; key_operations: string[] };
	availability: {
		enabled: boolean;
		available: boolean;
		availability_reason?: string | null;
		requires_connection: boolean;
		requires_permission: boolean;
	};
};

let manifests = new Map<string, ToolManifest>();

/** Replace the live catalog snapshot received from the backend. */
export function setToolManifests(entries: unknown): void {
	const next = new Map<string, ToolManifest>();
	if (Array.isArray(entries)) {
		for (const entry of entries) {
			const candidate =
				entry && typeof entry === 'object' && 'manifest' in entry
					? (entry as { manifest?: unknown }).manifest
					: entry;
			if (!isToolManifest(candidate)) continue;
			next.set(candidate.identity.stable_name, candidate);
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

function legacyRootName(toolName: string): string {
	const name = String(toolName || '');
	if (name === 'load_mcp' || name.startsWith('mcp__')) return 'mcp';
	if (name.startsWith('skill__')) return 'skill';
	return name.split('.')[0] || name;
}

function isToolManifest(value: unknown): value is ToolManifest {
	if (!value || typeof value !== 'object') return false;
	const manifest = value as Partial<ToolManifest>;
	return Boolean(
		manifest.identity &&
		typeof manifest.identity.stable_name === 'string' &&
		manifest.presentation &&
		typeof manifest.presentation.renderer === 'string' &&
		typeof manifest.presentation.icon === 'string',
	);
}
