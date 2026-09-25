import { invoke } from '$lib/tauri.ts';
import type {
	AddFactRequest,
	DeleteFactRequest,
	ListFactsRequest,
	RecallMemoryRequest,
} from './contracts/commands.ts';
import type { Fact, MemoryRecallItem } from './contracts/memory.ts';

/** List facts through the named memory command boundary. */
export function listFacts(request: ListFactsRequest): Promise<Fact[]> {
	return invoke('list_facts', request);
}

/** Store a user-managed fact through the named memory command boundary. */
export function addFact(request: AddFactRequest): Promise<Fact> {
	return invoke('add_fact', request);
}

/** Delete one fact through the named memory command boundary. */
export function deleteFact(request: DeleteFactRequest): Promise<void> {
	return invoke('delete_fact', request);
}

/** Recall facts or episodes through the named memory command boundary. */
export function recallMemory(request: RecallMemoryRequest): Promise<MemoryRecallItem[]> {
	return invoke('recall_memory', request);
}
