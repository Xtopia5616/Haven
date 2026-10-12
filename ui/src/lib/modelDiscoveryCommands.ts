import { invoke } from '$lib/tauri.ts';
import type { DiscoverModelsRequest } from './contracts/commands.ts';
import { parseDiscoveredModels, type DiscoveredModelView } from './contracts/model.ts';

/** Discover one provider's models using the existing flat Tauri arguments. */
export async function discoverModels(
	request: DiscoverModelsRequest,
): Promise<DiscoveredModelView[]> {
	return parseDiscoveredModels(await invoke('discover_models', request));
}
