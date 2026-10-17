import {
	TAURI_COMMAND_CONTRACTS,
	type CommandBoundary,
	type CommandContract,
	type TauriCommandName,
} from './generatedCommands.ts';

export { TAURI_COMMAND_CONTRACTS };
export type { CommandBoundary, CommandContract };

export const TAURI_COMMAND_NAMES = Object.keys(TAURI_COMMAND_CONTRACTS) as TauriCommandName[];
