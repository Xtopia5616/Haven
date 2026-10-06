<script lang="ts">
	import MaterialTabs from './MaterialTabs.svelte';
	import type { NavigationTab } from './navigationTypes.ts';

	interface Props {
		tabs?: NavigationTab[];
		activeTab?: string;
		onNavigate?: (tabId: string) => void;
	}

	let { tabs = [], activeTab = 'chat', onNavigate = () => {} }: Props = $props();
</script>

<nav aria-label="工作区导航">
	<MaterialTabs
		{tabs}
		{activeTab}
		{onNavigate}
		ariaLabel="工作区导航"
		idPrefix="workspace-tab"
		panelIdPrefix="workspace-tabpanel"
		showIcons
		className="workspace-nav"
	/>
</nav>
