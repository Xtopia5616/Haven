/**
 * Memory command response contracts at the renderer boundary.
 *
 * Rust `MemoryFactResponse`, `MemoryFactSourceRef`, and `MemoryRecallItem`
 * own the generated wire shapes. These renderer DTOs are explicitly mapped
 * after validation so Rust field names do not escape the command boundary.
 */
import {
	MEMORY_ENTITY_KIND_INPUT_VALUES,
	MEMORY_FACT_RESPONSE_SHAPE,
	MEMORY_FACT_SOURCE_INPUT_VALUES,
	MEMORY_RECALL_ITEM_RESPONSE_SHAPE,
	LIST_FACTS_RESPONSE_SHAPE,
} from './generatedCommands.ts';
import type {
	MemoryFactCursor as GeneratedMemoryFactCursor,
	MemoryFactPageResponse as GeneratedMemoryFactPageResponse,
	MemoryFactResponse as GeneratedMemoryFactResponse,
	MemoryRecallItem as GeneratedMemoryRecallItem,
	MemoryEntityKindInput,
	MemoryFactSourceInput,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { findGeneratedWireShapeMismatch } from './generatedWireShape.ts';
import { isOneOf } from './valueGuards.ts';
import { isCanonicalEntityId } from './wireGuards.ts';

export type MemoryFactSourceRef = Omit<
	NonNullable<GeneratedMemoryFactResponse['source_ref']>,
	'message_id'
> & { messageId: string };
export type Fact = Omit<GeneratedMemoryFactResponse, 'fact_id' | 'source_ref'> & {
	factId: string;
	sourceRef: MemoryFactSourceRef | null;
};
export type MemoryFactCursor = GeneratedMemoryFactCursor;
export interface FactPage {
	facts: Fact[];
	nextCursor: MemoryFactCursor | null;
}
export type MemoryRecallItem = Omit<GeneratedMemoryRecallItem, 'entity_id'> & {
	entityId: string;
};
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

function validateFactResponse(value: unknown): asserts value is GeneratedMemoryFactResponse {
	const mismatch = findGeneratedWireShapeMismatch(value, MEMORY_FACT_RESPONSE_SHAPE);
	if (mismatch) throw new Error(`Invalid memory fact response shape at ${mismatch}`);
	const fact = value as GeneratedMemoryFactResponse;
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
	return mapFactResponse(value as unknown as GeneratedMemoryFactResponse);
}

/** Validate and map one bounded fact page, including its stable keyset cursor. */
export function parseFactPageResponse(value: unknown): FactPage {
	if (!isRecord(value) || !Array.isArray(value.facts)) {
		throw new Error('Invalid memory fact page response shape');
	}
	if (!value.facts.every(hasFactIdentity)) throw new Error('Invalid memory fact identity');
	for (const fact of value.facts) validateFactResponse(fact);
	const mismatch = findGeneratedWireShapeMismatch(value, LIST_FACTS_RESPONSE_SHAPE);
	if (mismatch) throw new Error(`Invalid memory fact page response shape at ${mismatch}`);
	const page = value as unknown as GeneratedMemoryFactPageResponse;
	let nextCursor: MemoryFactCursor | null = null;
	if (page.next_cursor !== null) {
		if (
			!isCanonicalEntityId(page.next_cursor.factId, 'fact') ||
			!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)$/.test(
				page.next_cursor.createdAt,
			) ||
			!Number.isFinite(Date.parse(page.next_cursor.createdAt))
		) {
			throw new Error('Invalid memory fact cursor');
		}
		nextCursor = page.next_cursor;
	}
	return {
		facts: page.facts.map((fact) => mapFactResponse(fact)),
		nextCursor,
	};
}

function mapFactResponse(value: GeneratedMemoryFactResponse): Fact {
	const { fact_id, source_ref, ...rest } = value;
	let sourceRef: MemoryFactSourceRef | null = null;
	if (source_ref) {
		const { message_id, ...sourceRest } = source_ref;
		sourceRef = { ...sourceRest, messageId: message_id };
	}
	return {
		...rest,
		factId: fact_id,
		sourceRef,
	};
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
	const entityIds = new Set<string>();
	for (const item of value) {
		if (entityIds.has(item.entity_id)) throw new Error('Duplicate memory recall identity');
		entityIds.add(item.entity_id);
		const mismatch = findGeneratedWireShapeMismatch(item, MEMORY_RECALL_ITEM_RESPONSE_SHAPE);
		if (mismatch) throw new Error(`Invalid memory recall response shape at ${mismatch}`);
	}
	return value.map((item: GeneratedMemoryRecallItem) => {
		const { entity_id, ...rest } = item;
		return { ...rest, entityId: entity_id };
	});
}
