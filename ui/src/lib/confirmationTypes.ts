/** User decision emitted by ConfirmationDialog and handled by the app route. */
import type {
	PermissionEffectInput,
	PermissionScopeInput,
	PermissionTargetInput,
	PermissionResourceTargetInput,
} from '$lib/contracts/generatedCommands.ts';

export interface ConfirmationDecision {
	requestId: string;
	approved: boolean;
	effect?: PermissionEffectInput;
	scope?: PermissionScopeInput;
	target?: PermissionTargetInput;
	resourceTarget?: PermissionResourceTargetInput;
}
