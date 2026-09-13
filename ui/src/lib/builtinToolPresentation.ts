import { operationViewContract } from './operationViewContract.ts';
import { toolDisplayName } from './toolIdentity.ts';

export type BuiltinToolEntry = {
	name: string;
	desc: string;
	risk: string;
	schema: Record<string, unknown>;
	enabled: boolean;
	[key: string]: any;
};

export type BuiltinToolCard =
	| {
			kind: 'single';
			tool: BuiltinToolEntry;
	  }
	| {
			kind: 'operation-group';
			name: string;
			label: string;
			operations: BuiltinToolEntry[];
	  };

export type BuiltinEnabledFilter = 'all' | 'enabled' | 'disabled';

/**
 * Collapse only model-facing operation views into one UI card per capability
 * root. The original entries are kept by reference and retain their names,
 * schemas and enabled flags for the operation-level controls.
 */
export function groupBuiltinTools(tools: BuiltinToolEntry[]): BuiltinToolCard[] {
	const cards: BuiltinToolCard[] = [];
	const groups = new Map<string, Extract<BuiltinToolCard, { kind: 'operation-group' }>>();

	for (const tool of tools) {
		const view = operationViewContract(tool.name);
		if (!view) {
			cards.push({ kind: 'single', tool });
			continue;
		}

		let group = groups.get(view.root);
		if (!group) {
			group = {
				kind: 'operation-group',
				name: view.root,
				label: toolDisplayName(view.root),
				operations: [],
			};
			groups.set(view.root, group);
			cards.push(group);
		}
		group.operations.push(tool);
	}

	return cards;
}

function cardTools(card: BuiltinToolCard): BuiltinToolEntry[] {
	return card.kind === 'operation-group' ? card.operations : [card.tool];
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
		card.kind === 'operation-group' ? card.name : '',
		card.kind === 'operation-group' ? card.label : '',
		...tools.flatMap((tool) => [tool.name, tool.desc]),
	]
		.filter(Boolean)
		.join(' ')
		.toLocaleLowerCase();
	return text.includes(normalizedQuery);
}
