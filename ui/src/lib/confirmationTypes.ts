/** User decision emitted by ConfirmationDialog and handled by the app route. */
export interface ConfirmationDecision {
	stepId: string;
	approved: boolean;
	effect?: string;
	scope?: string;
	target?: string;
}
