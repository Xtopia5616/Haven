import { describe, expect, it } from 'vitest';
import { resolveSettingsSaveAction } from './settingsSaveAction';

describe('resolveSettingsSaveAction', () => {
	it('waits while settings are loading', () => {
		expect(resolveSettingsSaveAction(false, false)).toBe('loading');
		expect(resolveSettingsSaveAction(false, true)).toBe('loading');
	});

	it('reports an empty save when there are no changes', () => {
		expect(resolveSettingsSaveAction(true, false)).toBe('empty');
	});

	it('starts saving when settings are dirty', () => {
		expect(resolveSettingsSaveAction(true, true)).toBe('save');
	});
});
