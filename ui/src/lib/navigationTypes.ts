/** Shared renderer shape consumed by workspace and page tab navigators. */
export interface NavigationTab {
	id: string;
	label: string;
	hint?: string;
	icon?: string;
}

/** Shared selection contract for components that render NavigationTab lists. */
export interface NavigationTabsProps {
	tabs?: NavigationTab[];
	activeTab?: NavigationTab['id'];
	onNavigate?: (tabId: NavigationTab['id']) => void;
}
