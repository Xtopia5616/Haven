<script>
	import MaterialTabs from './MaterialTabs.svelte';

	let { tabs = [], activeTab = 'chat', onNavigate = () => {} } = $props();
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
		indicator="css"
		className="workspace-nav"
	/>
</nav>
