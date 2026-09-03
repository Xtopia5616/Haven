export type SettingsSaveAction = 'loading' | 'empty' | 'save';

/** Resolve the user-visible action for the settings save affordance. */
export function resolveSettingsSaveAction(
	settingsLoaded: boolean,
	settingsDirty: boolean,
): SettingsSaveAction {
	if (!settingsLoaded) return 'loading';
	return settingsDirty ? 'save' : 'empty';
}
