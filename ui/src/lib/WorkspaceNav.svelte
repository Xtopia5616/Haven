<script lang="ts">
	import MaterialTabs from './MaterialTabs.svelte';

	interface TabItem {
		id: string;
		label: string;
		hint?: string;
		icon?: string;
	}

	interface Props {
		tabs?: TabItem[];
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
		indicator="measured"
		className="workspace-nav"
	/>
</nav>
