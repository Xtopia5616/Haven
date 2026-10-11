/**
 * Memory command response contracts at the renderer boundary.
 *
 * Rust `MemoryFactResponse`, `MemoryFactSourceRef`, `MemoryRecallItem`, and
 * `MemoryEntityKind` own the generated wire shapes. The UI-facing aliases
 * retain snake_case because memory views consume those names directly.
 */
import {
	MEMORY_ENTITY_KIND_INPUT_VALUES,
	MEMORY_FACT_SOURCE_INPUT_VALUES,
} from './generatedCommands.ts';
import type {
	MemoryFactResponse as GeneratedMemoryFactResponse,
	MemoryRecallItem as GeneratedMemoryRecallItem,
	MemoryEntityKindInput,
	MemoryFactSourceInput,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isOneOf } from './valueGuards.ts';
import { isCanonicalEntityId } from './wireGuards.ts';

export type Fact = GeneratedMemoryFactResponse;
export type MemoryRecallItem = GeneratedMemoryRecallItem;
export type MemoryRecallKind = MemoryEntityKindInput;
export type MemoryFactSourceFilter = MemoryFactSourceInput | '';
export const MEMORY_RECALL_FILTER_VALUES = [
	'all',
	...MEMORY_ENTITY_KIND_INPUT_VALUES,
] as const satisfies readonly ('all' | MemoryRecallKind)[];
export type MemoryRecallFilter = (typeof MEMORY_RECALL_FILTER_VALUES)[number];

export function isMemoryFactSourceInput(value: string): value is MemoryFactSourceInput {
	return isOneOf(value, MEMORY_FACT_SOURCE_INPUT_VALUES);
}

export function isMemoryRecallFilter(value: string): value is MemoryRecallFilter {
	return isOneOf(value, MEMORY_RECALL_FILTER_VALUES);
}

/** Recall item with the requested kind attached by the existing UI projection. */
export type MemoryRecallResult = MemoryRecallItem & { kind: MemoryRecallKind };

export interface MemoryRecallState {
	query: string;
	kind: MemoryRecallFilter;
	results: MemoryRecallResult[];
	loading: boolean;
	searched: boolean;
}

function hasFactIdentity(value: unknown): boolean {
	return (
		isRecord(value) &&
		isCanonicalEntityId(value.fact_id, 'fact') &&
		(value.source_ref === null ||
			(isRecord(value.source_ref) && isCanonicalEntityId(value.source_ref.message_id, 'msg')))
	);
}

/** Validate a fact row and its optional source-message identity. */
export function parseFactResponse(value: unknown): Fact {
	if (!hasFactIdentity(value)) throw new Error('Invalid memory fact identity');
	return value as unknown as Fact;
}

/** Validate fact identities before listing the persisted fact catalog. */
export function parseFactResponses(value: unknown): Fact[] {
	if (!Array.isArray(value) || !value.every(hasFactIdentity)) {
		throw new Error('Invalid memory fact identity');
	}
	return value as unknown as Fact[];
}

/** Validate recall entity IDs against the exact kind requested from Rust. */
export function parseMemoryRecallResponses(
	value: unknown,
	kind: MemoryRecallKind | null | undefined,
): MemoryRecallItem[] {
	const prefix = kind === 'episode' ? 'msg' : 'fact';
	if (
		!Array.isArray(value) ||
		!value.every((item) => isRecord(item) && isCanonicalEntityId(item.entity_id, prefix))
	) {
		throw new Error('Invalid memory recall identity');
	}
	return value as unknown as MemoryRecallItem[];
}
