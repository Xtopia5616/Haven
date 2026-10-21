import {
	NETWORK_POLICY_VALUES,
	SANDBOX_MODE_VALUES,
	SHELL_BROKER_GATE_STATUS_VALUES,
	type ShellExecutionSecurityStatus as GeneratedShellExecutionSecurityStatus,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isOneOf } from './valueGuards.ts';

export type ShellExecutionSecurityStatus = GeneratedShellExecutionSecurityStatus;

/** Validate the active Shell policy projection before using it in security UI. */
export function parseShellExecutionSecurityStatus(
	value: unknown,
): ShellExecutionSecurityStatus {
	if (
		!isRecord(value) ||
		!isOneOf(value.sandboxMode, SANDBOX_MODE_VALUES) ||
		!isOneOf(value.networkPolicy, NETWORK_POLICY_VALUES) ||
		!isOneOf(value.brokerGateStatus, SHELL_BROKER_GATE_STATUS_VALUES)
	) {
		throw new Error('invalid get_shell_execution_security_status response');
	}
	return {
		sandboxMode: value.sandboxMode,
		networkPolicy: value.networkPolicy,
		brokerGateStatus: value.brokerGateStatus,
	} as ShellExecutionSecurityStatus;
}
