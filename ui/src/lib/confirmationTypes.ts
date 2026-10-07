/** User decision emitted by ConfirmationDialog and handled by the app route. */
import type {
	PermissionEffectInput,
	PermissionScopeInput,
	PermissionTargetInput,
} from '$lib/contracts/generatedCommands.ts';

export interface ConfirmationDecision {
	stepId: string;
	approved: boolean;
	effect?: PermissionEffectInput;
	scope?: PermissionScopeInput;
	target?: PermissionTargetInput;
}
