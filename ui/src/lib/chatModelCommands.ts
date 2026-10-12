import type { TauriCommandRequest } from './contracts/generatedCommands.ts';
import { invoke } from './tauri.ts';

/** Select the configured model profile for the Chat request. */
export function switchModel(request: TauriCommandRequest<'switch_model'>): Promise<void> {
	return invoke('switch_model', request);
}

/** Set the Chat request's reasoning effort override. */
export function setReasoningEffort(
	request: TauriCommandRequest<'set_reasoning_effort'>,
): Promise<void> {
	return invoke('set_reasoning_effort', request);
}

/** Set or clear the Chat request's built-in web-search override. */
export function setWebSearch(request: TauriCommandRequest<'set_web_search'>): Promise<void> {
	return invoke('set_web_search', request);
}
