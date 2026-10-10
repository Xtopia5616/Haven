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
import { isOneOf } from './valueGuards.ts';

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
