import { hasIcon, type IconName } from './icons.ts';

/** Shared renderer shape consumed by workspace and page tab navigators. */
export interface NavigationTab {
	id: string;
	label: string;
	hint?: string;
	icon?: IconName;
}

/** Resolve the optional tab icon without treating arbitrary tab ids as registry keys. */
export function navigationTabIconName(tab: NavigationTab): IconName {
	if (tab.icon) return hasIcon(tab.icon) ? tab.icon : 'help';
	const fallbackId = tab.id || 'settings';
	return hasIcon(fallbackId) ? fallbackId : 'help';
}

/** Shared selection contract for components that render NavigationTab lists. */
export interface NavigationTabsProps {
	tabs?: NavigationTab[];
	activeTab?: NavigationTab['id'];
	onNavigate?: (tabId: NavigationTab['id']) => void;
}
