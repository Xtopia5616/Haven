import { invoke } from '$lib/tauri.ts';
import type { DiscoverModelsRequest } from './contracts/commands.ts';
import type { DiscoveredModelMap, ModelInfo } from './contracts/model.ts';

/** Discover one provider's models using the existing flat Tauri arguments. */
export function discoverModels(request: DiscoverModelsRequest): Promise<ModelInfo[]> {
	return invoke('discover_models', request);
}

/** Discover the configured providers' model lists. */
export function discoverAllModels(): Promise<DiscoveredModelMap> {
	return invoke('discover_all_models');
}
