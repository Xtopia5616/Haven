import {
	parseShellExecutionSecurityStatus,
	type ShellExecutionSecurityStatus,
} from '$lib/contracts/shellExecution.ts';
import { invoke } from '$lib/tauri.ts';

/** Read the active Shell boundary and authenticated broker gate summary. */
export function readShellExecutionSecurityStatus(): Promise<ShellExecutionSecurityStatus> {
	return invoke('get_shell_execution_security_status').then(parseShellExecutionSecurityStatus);
}
