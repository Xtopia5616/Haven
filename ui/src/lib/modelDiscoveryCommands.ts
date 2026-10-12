import { invoke } from '$lib/tauri.ts';
import type { TauriCommandRequest } from './contracts/generatedCommands.ts';
import { parseDiscoveredModels, type DiscoveredModelView } from './contracts/model.ts';

/** Discover one provider's models using the existing flat Tauri arguments. */
export async function discoverModels(
	request: TauriCommandRequest<'discover_models'>,
): Promise<DiscoveredModelView[]> {
	return parseDiscoveredModels(await invoke('discover_models', request));
}
