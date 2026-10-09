import type {
	SetReasoningEffortRequest,
	SetWebSearchRequest,
	SwitchModelRequest,
} from './contracts/commands.ts';
import { invoke } from './tauri.ts';

/** Select the configured model profile for the Chat request. */
export function switchModel(request: SwitchModelRequest): Promise<void> {
	return invoke('switch_model', request);
}

/** Set the Chat request's reasoning effort override. */
export function setReasoningEffort(request: SetReasoningEffortRequest): Promise<void> {
	return invoke('set_reasoning_effort', request);
}

/** Set or clear the Chat request's built-in web-search override. */
export function setWebSearch(request: SetWebSearchRequest): Promise<void> {
	return invoke('set_web_search', request);
}
