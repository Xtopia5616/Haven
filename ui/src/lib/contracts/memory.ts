/**
 * Memory command response contracts at the renderer boundary.
 *
 * `haven_memory::repositories::facts::Fact` and
 * `app_binary::commands::contracts::MemoryRecallItem` own the Rust wire
 * shapes. These DTOs deliberately retain their existing snake_case fields;
 * the current memory views consume those names and command responses are
 * passed through without normalization.
 */
export interface FactSourceRef {
	message_id: string;
	snippet: string;
	[field: string]: unknown;
}

export interface Fact {
	id: string;
	subject: string;
	predicate: string;
	object: string;
	source: string;
	confidence: number;
	tags: string[];
	created_at: string;
	mention_count: number;
	last_seen_at: string | null;
	source_ref: FactSourceRef | null;
	durability: number;
	[field: string]: unknown;
}

export interface MemoryRecallItem {
	entity_id: string;
	text: string;
	score: number;
	model: string;
	[field: string]: unknown;
}

/** Recall item with the requested kind attached by the existing UI projection. */
export type MemoryRecallResult = MemoryRecallItem & { kind: string };

export interface MemoryRecallState {
	query: string;
	kind: string;
	results: MemoryRecallResult[];
	loading: boolean;
	searched: boolean;
}
