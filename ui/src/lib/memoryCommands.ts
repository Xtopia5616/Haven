import { invoke } from '$lib/tauri.ts';
import type {
	AddFactRequest,
	DeleteFactRequest,
	ListFactsRequest,
	RecallMemoryRequest,
} from './contracts/commands.ts';
import { parseCountResponse } from './contracts/count.ts';
import {
	parseFactResponse,
	parseFactResponses,
	parseMemoryRecallResponses,
	type Fact,
	type MemoryRecallItem,
} from './contracts/memory.ts';

/** List facts through the named memory command boundary. */
export async function listFacts(request: ListFactsRequest): Promise<Fact[]> {
	const value: unknown = await invoke('list_facts', request);
	return parseFactResponses(value);
}

/** Store a user-managed fact through the named memory command boundary. */
export async function addFact(request: AddFactRequest): Promise<Fact> {
	const value: unknown = await invoke('add_fact', request);
	return parseFactResponse(value);
}

/** Delete one fact through the named memory command boundary. */
export function deleteFact(request: DeleteFactRequest): Promise<void> {
	return invoke('delete_fact', request);
}

/** Delete every saved long-term fact. */
export function clearFacts(): Promise<number> {
	return invoke('clear_facts').then((value: unknown) => parseCountResponse(value, 'clear_facts'));
}

/** Recall facts or episodes through the named memory command boundary. */
export async function recallMemory(request: RecallMemoryRequest): Promise<MemoryRecallItem[]> {
	const value: unknown = await invoke('recall_memory', request);
	return parseMemoryRecallResponses(value, request.kind);
}
