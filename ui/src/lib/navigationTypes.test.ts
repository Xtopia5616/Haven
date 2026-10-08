import { describe, expect, it } from 'vitest';
import { navigationTabIconName, type NavigationTab } from './navigationTypes.ts';

describe('navigationTabIconName', () => {
	it('uses a registered explicit icon', () => {
		expect(navigationTabIconName({ id: 'workspace', label: '工作区', icon: 'briefcase' })).toBe(
			'briefcase',
		);
	});

	it('uses a registered tab id when no icon is provided', () => {
		expect(navigationTabIconName({ id: 'chat', label: '对话' })).toBe('chat');
	});

	it('uses the settings icon when the tab id is empty', () => {
		expect(navigationTabIconName({ id: '', label: '空页签' })).toBe('settings');
	});

	it('uses the help icon for an unregistered tab id', () => {
		expect(navigationTabIconName({ id: 'custom-page', label: '自定义页' })).toBe('help');
	});

	it('falls back safely for invalid runtime icon metadata', () => {
		const tab = { id: 'chat', label: '对话', icon: 'not-registered' } as unknown as NavigationTab;
		expect(navigationTabIconName(tab)).toBe('help');
	});
});
