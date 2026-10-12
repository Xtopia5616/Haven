import type { TauriCommandRequest } from './contracts/generatedCommands.ts';
import { invoke } from './tauri.ts';

/** Open a validated URL or local path through the reviewed Tauri command. */
export function openExternalTarget(
	request: TauriCommandRequest<'open_external'>,
): Promise<void> {
	return invoke('open_external', request);
}
