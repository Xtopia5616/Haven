import type { ToolManifest } from './toolManifest.ts';

export type BuiltinToolEntry = {
	name: string;
	desc: string;
	risk: string;
	category?: string;
	root?: string;
	rootLabel?: string;
	rootDescription?: string;
	rootIcon?: string;
	operation?: string | null;
	schema: Record<string, unknown>;
	enabled: boolean;
	[key: string]: any;
};

export type BuiltinToolRootCard = {
	kind: 'root-group';
	name: string;
	label: string;
	description?: string;
	icon?: string;
	operations: BuiltinToolEntry[];
};

export type BuiltinToolCard = {
	kind: 'family-group';
	name: string;
	label: string;
	roots: BuiltinToolRootCard[];
};

export type BuiltinEnabledFilter = 'all' | 'enabled' | 'disabled';

/** Project one canonical manifest into the view model used by the settings UI. */
export function builtinToolEntryFromManifest(manifest: ToolManifest): BuiltinToolEntry {
	return {
		name: manifest.identity.stableName,
		label: manifest.presentation.label,
		desc: manifest.model.description,
		risk: manifest.policy.riskLevel,
		category: manifest.identity.catalogGroup,
		root: manifest.identity.root,
		rootLabel: manifest.rootPresentation.label,
		rootDescription: manifest.rootPresentation.description,
		rootIcon: manifest.rootPresentation.icon,
		operation: manifest.identity.operation,
		schema: (manifest.model.inputSchema || {}) as Record<string, unknown>,
		enabled: manifest.availability.enabled,
		available: manifest.availability.available,
		availabilityReason: manifest.availability.availabilityReason,
		manifest,
	};
}

/**
 * Collapse all builtin tools into one UI card per backend-owned catalog group.
 * The original entries are kept by reference and retain their names, schemas
 * and enabled flags for the operation-level controls.
 */
export function groupBuiltinTools(tools: BuiltinToolEntry[]): BuiltinToolCard[] {
	const groups = new Map<string, BuiltinToolCard>();

	for (const tool of tools) {
		const category = tool.category || 'other';
		let group = groups.get(category);
		if (!group) {
			group = {
				kind: 'family-group',
				name: category,
				label: categoryLabel(category),
				roots: [],
			};
			groups.set(category, group);
		}

		const rootName = tool.root || legacyRootName(tool.name);
		let root = group.roots.find((candidate) => candidate.name === rootName);
		if (!root) {
			root = {
				kind: 'root-group',
				name: rootName,
				label: tool.rootLabel || rootName,
				description: tool.rootDescription || '',
				icon: tool.rootIcon || 'tools',
				operations: [],
			};
			group.roots.push(root);
		}
		root.operations.push(tool);
	}

	for (const group of groups.values()) {
		group.roots.sort(
			(a, b) => a.name.localeCompare(b.name),
		);
		for (const root of group.roots) {
			root.operations.sort((a, b) => a.name.localeCompare(b.name));
		}
	}

	return [...groups.values()].sort(
		(a, b) => categoryOrder(a.name) - categoryOrder(b.name) || a.name.localeCompare(b.name),
	);
}

function categoryOrder(category: string): number {
	return { haven: 0, system: 1, agent: 2, skills: 3, mcp: 4, other: 5 }[category] ?? 99;
}

function categoryLabel(category: string): string {
	return {
		haven: 'Haven',
		system: 'System',
		agent: 'Agent',
		skills: 'Skills',
		mcp: 'MCP',
		other: 'Other',
	}[category] || category;
}

function normalizedQuery(query: string): string {
	return query.trim().toLocaleLowerCase();
}

function matchesStatus(tool: BuiltinToolEntry, enabledFilter: string): boolean {
	if (enabledFilter === 'enabled') return tool.enabled;
	if (enabledFilter === 'disabled') return !tool.enabled;
	return true;
}

function matchesText(text: string, query: string): boolean {
	return Boolean(query && text.toLocaleLowerCase().includes(query));
}

function rootMatches(root: BuiltinToolRootCard, query: string): boolean {
	return matchesText(root.name, query) || matchesText(root.label, query);
}

function operationMatches(operation: BuiltinToolEntry, query: string): boolean {
	return (
		matchesText(operation.name, query) ||
		matchesText(operation.desc, query) ||
		matchesText(operation.operation || '', query)
	);
}

/**
 * Return a filtered copy of a family card while preserving the three-level
 * shape. A query matching a family/root keeps all of that node's operations;
 * otherwise only matching operations are retained.
 */
export function filterBuiltinToolCard(
	card: BuiltinToolCard,
	query: string,
	enabledFilter: string,
): BuiltinToolCard | null {
	const normalized = normalizedQuery(query);
	const familyMatches =
		!normalized || matchesText(card.name, normalized) || matchesText(card.label, normalized);
	const roots = card.roots
		.map((root) => {
			const rootMatchesQuery = familyMatches || !normalized || rootMatches(root, normalized);
			const operations = root.operations.filter(
				(operation) =>
					matchesStatus(operation, enabledFilter) &&
					(rootMatchesQuery || operationMatches(operation, normalized)),
			);
			return operations.length > 0 ? { ...root, operations } : null;
		})
		.filter((root): root is BuiltinToolRootCard => root !== null);

	return roots.length > 0 ? { ...card, roots } : null;
}

/**
 * Match a collapsed card against the shared resource filters. A group is
 * visible for an enabled/disabled filter when it contains at least one
 * matching operation, so mixed-state groups remain actionable.
 */
export function matchesBuiltinToolCard(
	card: BuiltinToolCard,
	query: string,
	enabledFilter: string,
): boolean {
	return filterBuiltinToolCard(card, query, enabledFilter) !== null;
}

function legacyRootName(toolName: string): string {
	return toolName.split('.')[0] || toolName;
}
