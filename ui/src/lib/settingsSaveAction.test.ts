import { describe, expect, it } from 'vitest';
import { selectSettingsSaveAction } from './settingsSaveAction';

describe('selectSettingsSaveAction', () => {
	it('waits while settings are loading', () => {
		expect(selectSettingsSaveAction(false, false)).toBe('loading');
		expect(selectSettingsSaveAction(false, true)).toBe('loading');
	});

	it('reports an empty save when there are no changes', () => {
		expect(selectSettingsSaveAction(true, false)).toBe('empty');
	});

	it('starts saving when settings are dirty', () => {
		expect(selectSettingsSaveAction(true, true)).toBe('save');
	});
});
