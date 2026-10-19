export type SettingsSaveAction = 'loading' | 'empty' | 'save';

/** Select the user-visible action for the settings save affordance. */
export function selectSettingsSaveAction(
	settingsLoaded: boolean,
	settingsDirty: boolean,
): SettingsSaveAction {
	if (!settingsLoaded) return 'loading';
	return settingsDirty ? 'save' : 'empty';
}
