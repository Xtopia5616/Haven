export type SettingsLeaveGuard = {
	/** True when the settings form has unsaved edits (evaluated lazily). */
	isDirty: () => boolean;
	/** Prompt the user; resolve true to allow leaving the settings tab. */
	confirmLeave: () => Promise<boolean>;
};

let guard: SettingsLeaveGuard | null = null;

/** @param {SettingsLeaveGuard | null} g */
export function registerSettingsLeaveGuard(g: SettingsLeaveGuard | null) {
	guard = g;
}

/**
 * If settings are dirty, run the registered leave prompt.
 * Dirty is checked lazily so the settings form does not stringify on every keystroke.
 * @returns {Promise<boolean>} true when navigation away may proceed
 */
export async function confirmLeaveSettingsIfNeeded(): Promise<boolean> {
	if (!guard || !guard.isDirty()) return true;
	return guard.confirmLeave();
}
