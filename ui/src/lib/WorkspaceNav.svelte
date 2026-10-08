<script lang="ts">
	import MaterialTabs from './MaterialTabs.svelte';
	import type { NavigationTabsProps } from './navigationTypes.ts';

	let { tabs = [], activeTab = 'chat', onNavigate = () => {} }: NavigationTabsProps = $props();
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
