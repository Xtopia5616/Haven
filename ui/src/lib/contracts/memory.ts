/**
 * Memory command response contracts at the renderer boundary.
 *
 * `haven_memory::repositories::facts::Fact` and
 * `app_binary::commands::contracts::MemoryRecallItem` own the Rust wire
 * shapes. These DTOs deliberately retain their existing snake_case fields;
 * the current memory views consume those names and command responses are
 * passed through without normalization.
 */
import type {
	Fact as GeneratedFact,
	FactSourceRef as GeneratedFactSourceRef,
	MemoryRecallItem as GeneratedMemoryRecallItem,
} from './generatedCommands.ts';

export type FactSourceRef = GeneratedFactSourceRef;
export type Fact = GeneratedFact;
export type MemoryRecallItem = GeneratedMemoryRecallItem;

/** Recall item with the requested kind attached by the existing UI projection. */
export type MemoryRecallResult = MemoryRecallItem & { kind: string };

export interface MemoryRecallState {
	query: string;
	kind: string;
	results: MemoryRecallResult[];
	loading: boolean;
	searched: boolean;
}
