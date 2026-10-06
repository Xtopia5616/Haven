/**
 * Memory command response contracts at the renderer boundary.
 *
 * Rust `MemoryFactResponse`, `MemoryFactSourceRef`, and
 * `MemoryRecallItem` own the generated wire shapes. The UI-facing aliases
 * retain snake_case because memory views consume those names directly.
 */
import type {
	MemoryFactResponse as GeneratedMemoryFactResponse,
	MemoryRecallItem as GeneratedMemoryRecallItem,
} from './generatedCommands.ts';

export type Fact = GeneratedMemoryFactResponse;
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
