/** Shared renderer shape consumed by workspace and page tab navigators. */
export interface NavigationTab {
	id: string;
	label: string;
	hint?: string;
	icon?: string;
}
