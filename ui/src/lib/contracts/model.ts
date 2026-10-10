/** Model metadata returned by the Rust ModelInfo wire DTO. */
import type { ModelInfo as GeneratedModelInfo } from './generatedCommands.ts';

export type ModelInfo = GeneratedModelInfo;
/** Last successful catalogs cached by configured Provider connection name. */
export type DiscoveredModelsByProviderName = Record<string, ModelInfo[]>;
