/** Model metadata returned by the Rust ModelInfo wire DTO. */
import type { ModelInfo as GeneratedModelInfo, TauriCommandResponse } from './generatedCommands.ts';

export type ModelInfo = GeneratedModelInfo;
export type DiscoveredModelMap = TauriCommandResponse<'discover_all_models'>;
