import { invoke } from '$lib/tauri.ts';
import type { TauriCommandRequest } from './contracts/generatedCommands.ts';
import { parseCountResponse } from './contracts/count.ts';
import {
	parseFactResponse,
	parseFactPageResponse,
	parseFactProposalPageResponse,
	parseFactProposalReviewResponse,
	parseMemoryRecallResponses,
	type Fact,
	type FactPage,
	type FactProposalDecision,
	type FactProposalPage,
	type FactProposalReviewResult,
	type MemoryRecallItem,
} from './contracts/memory.ts';

/** List facts through the named memory command boundary. */
export async function listFacts(request: TauriCommandRequest<'list_facts'>): Promise<FactPage> {
	const value: unknown = await invoke('list_facts', request);
	return parseFactPageResponse(value);
}

/** List a bounded page of inferred facts awaiting explicit user review. */
export async function listFactProposals(
	request: TauriCommandRequest<'list_fact_proposals'>,
): Promise<FactProposalPage> {
	const value: unknown = await invoke('list_fact_proposals', request);
	return parseFactProposalPageResponse(value);
}

/** Accept or reject one pending inference exactly once. */
export async function reviewFactProposal(
	request: Omit<TauriCommandRequest<'review_fact_proposal'>, 'decision'> & {
		decision: FactProposalDecision;
	},
): Promise<FactProposalReviewResult> {
	const value: unknown = await invoke('review_fact_proposal', request);
	return parseFactProposalReviewResponse(value);
}

/** Store a user-managed fact through the named memory command boundary. */
export async function addFact(request: TauriCommandRequest<'add_fact'>): Promise<Fact> {
	const value: unknown = await invoke('add_fact', request);
	return parseFactResponse(value);
}

/** Delete one fact through the named memory command boundary. */
export function deleteFact(request: TauriCommandRequest<'delete_fact'>): Promise<void> {
	return invoke('delete_fact', request);
}

/** Delete every saved long-term fact. */
export function clearFacts(): Promise<number> {
	return invoke('clear_facts').then((value: unknown) => parseCountResponse(value, 'clear_facts'));
}

/** Recall facts or episodes through the named memory command boundary. */
export async function recallMemory(
	request: TauriCommandRequest<'recall_memory'>,
): Promise<MemoryRecallItem[]> {
	const value: unknown = await invoke('recall_memory', request);
	return parseMemoryRecallResponses(value, request.kind);
}
