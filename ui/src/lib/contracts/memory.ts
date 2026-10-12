/**
 * Memory command response contracts at the renderer boundary.
 *
 * Rust `MemoryFactResponse`, `MemoryFactSourceRef`, `MemoryRecallItem`, and
 * `MemoryEntityKind` own the generated wire shapes. The UI-facing aliases
 * retain snake_case because memory views consume those names directly.
 */
import {
	MEMORY_ENTITY_KIND_INPUT_VALUES,
	MEMORY_FACT_RESPONSE_SHAPE,
	MEMORY_FACT_SOURCE_INPUT_VALUES,
	MEMORY_RECALL_ITEM_RESPONSE_SHAPE,
} from './generatedCommands.ts';
import type {
	MemoryFactResponse as GeneratedMemoryFactResponse,
	MemoryRecallItem as GeneratedMemoryRecallItem,
	MemoryEntityKindInput,
	MemoryFactSourceInput,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { findGeneratedWireShapeMismatch } from './generatedWireShape.ts';
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
	if (!isRecord(value) || !isCanonicalEntityId(value.fact_id, 'fact')) return false;
	if (!Object.prototype.hasOwnProperty.call(value, 'source_ref')) return true;
	if (value.source_ref === null || !isRecord(value.source_ref)) return true;
	if (!Object.prototype.hasOwnProperty.call(value.source_ref, 'message_id')) return true;
	return isCanonicalEntityId(value.source_ref.message_id, 'msg');
}

function validateFactResponse(value: unknown): asserts value is Fact {
	const mismatch = findGeneratedWireShapeMismatch(value, MEMORY_FACT_RESPONSE_SHAPE);
	if (mismatch) throw new Error(`Invalid memory fact response shape at ${mismatch}`);
	const fact = value as Fact;
	if (
		fact.confidence < 0 ||
		fact.confidence > 1 ||
		fact.durability < 0 ||
		fact.durability > 1 ||
		!Number.isSafeInteger(fact.mention_count) ||
		fact.mention_count < 0
	) {
		throw new Error('Invalid memory fact response value');
	}
}

/** Validate the complete fact row and its domain-specific identities/values. */
export function parseFactResponse(value: unknown): Fact {
	if (!hasFactIdentity(value)) throw new Error('Invalid memory fact identity');
	validateFactResponse(value);
	return value as unknown as Fact;
}

/** Validate complete fact rows before listing the persisted fact catalog. */
export function parseFactResponses(value: unknown): Fact[] {
	if (!Array.isArray(value) || !value.every(hasFactIdentity)) {
		throw new Error('Invalid memory fact identity');
	}
	for (const fact of value) validateFactResponse(fact);
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
	for (const item of value) {
		const mismatch = findGeneratedWireShapeMismatch(item, MEMORY_RECALL_ITEM_RESPONSE_SHAPE);
		if (mismatch) throw new Error(`Invalid memory recall response shape at ${mismatch}`);
	}
	return value as unknown as MemoryRecallItem[];
}
