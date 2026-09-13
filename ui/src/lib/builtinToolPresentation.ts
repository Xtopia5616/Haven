export type BuiltinToolEntry = {
	name: string;
	desc: string;
	risk: string;
	category?: string;
	schema: Record<string, unknown>;
	enabled: boolean;
	[key: string]: any;
};

export type BuiltinToolCard = {
	kind: 'category-group';
	name: string;
	label: string;
	operations: BuiltinToolEntry[];
};

export type BuiltinEnabledFilter = 'all' | 'enabled' | 'disabled';

/**
 * Collapse all builtin tools into one UI card per backend-owned catalog group.
 * The original entries are kept by reference and retain their names, schemas
 * and enabled flags for the operation-level controls.
 */
export function groupBuiltinTools(tools: BuiltinToolEntry[]): BuiltinToolCard[] {
	const groups = new Map<string, Extract<BuiltinToolCard, { kind: 'category-group' }>>();

	for (const tool of tools) {
		const category = tool.category || 'other';
		let group = groups.get(category);
		if (!group) {
			group = {
				kind: 'category-group',
				name: category,
				label: categoryLabel(category),
				operations: [],
			};
			groups.set(category, group);
		}
		group.operations.push(tool);
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

function cardTools(card: BuiltinToolCard): BuiltinToolEntry[] {
	return card.operations;
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
	const tools = cardTools(card);
	if (enabledFilter === 'enabled' && !tools.some((tool) => tool.enabled)) return false;
	if (enabledFilter === 'disabled' && !tools.some((tool) => !tool.enabled)) return false;

	const normalizedQuery = query.trim().toLocaleLowerCase();
	if (!normalizedQuery) return true;

	const text = [
		card.name,
		card.label,
		...tools.flatMap((tool) => [tool.name, tool.desc]),
	]
		.filter(Boolean)
		.join(' ')
		.toLocaleLowerCase();
	return text.includes(normalizedQuery);
}
