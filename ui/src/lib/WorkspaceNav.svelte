<script>
	import { onMount, tick } from 'svelte';

	let { tabs = [], activeTab = 'chat', onNavigate = () => {} } = $props();
	/** @type {HTMLDivElement | undefined} */
	let navElement;
	/** @type {Record<string, HTMLButtonElement>} */
	let tabElements = $state({});
	let indicator = $state({ x: 0, width: 24, visible: false });
	/** @type {ResizeObserver | undefined} */
	let resizeObserver;

	function updateIndicator() {
		const activeElement = tabElements[activeTab];
		if (!navElement || !activeElement) return;

		const navRect = navElement.getBoundingClientRect();
		const tabRect = activeElement.getBoundingClientRect();
		const width =
			Number.parseFloat(
				getComputedStyle(navElement).getPropertyValue('--md-comp-tab-indicator-min-width'),
			) || 24;

		if (tabRect.width <= 0) {
			indicator.visible = false;
			return;
		}

		indicator = {
			x: tabRect.left - navRect.left + (tabRect.width - width) / 2,
			width,
			visible: true,
		};
	}

	$effect(() => {
		activeTab;
		tabs;
		void tick().then(updateIndicator);
	});

	onMount(() => {
		const handleResize = () => updateIndicator();
		window.addEventListener('resize', handleResize);
		if (typeof ResizeObserver !== 'undefined' && navElement) {
			const observer = new ResizeObserver(handleResize);
			resizeObserver = observer;
			observer.observe(navElement);
			Object.values(tabElements).forEach((element) => observer.observe(element));
		}
		updateIndicator();

		return () => {
			window.removeEventListener('resize', handleResize);
			resizeObserver?.disconnect();
		};
	});
</script>

<nav aria-label="工作区导航">
	<div
		bind:this={navElement}
		class="workspace-nav md-tabs"
		class:workspace-nav--indicator-ready={indicator.visible}
		style={`--md-tab-indicator-x: ${indicator.x}px; --md-tab-indicator-width: ${indicator.width}px;`}
		role="tablist"
	>
		{#each tabs as tab (tab.id)}
			<button
				type="button"
				id="workspace-tab-{tab.id}"
				class="md-tab"
				class:active={activeTab === tab.id}
				bind:this={tabElements[tab.id]}
				aria-selected={activeTab === tab.id}
				aria-controls="workspace-tabpanel-{tab.id}"
				role="tab"
				onclick={() => onNavigate(tab.id)}
			>
				<span class="md-tab__icon" aria-hidden="true">
					{#if tab.id === 'chat'}
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path
								d="M5.5 4.5h10A3.5 3.5 0 0 1 19 8v4.25a3.5 3.5 0 0 1-3.5 3.5H11l-5.5 4v-4.04a3.5 3.5 0 0 1-3.5-3.46V8a3.5 3.5 0 0 1 3.5-3.5Z"
							/>
							<path d="M7 9h7M7 12h4" /></svg
						>
					{:else if tab.id === 'tasks'}
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="1.8"
							stroke-linecap="round"
							stroke-linejoin="round"
							><rect x="4" y="3.5" width="16" height="17" rx="3" /><path
								d="M8 8h.01M11.5 8H16M8 12h.01M11.5 12H16M8 16h.01M11.5 16H14"
							/></svg
						>
					{:else if tab.id === 'tools'}
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path
								d="M4 9h16v10.5H4zM8 9V6.75A1.75 1.75 0 0 1 9.75 5h4.5A1.75 1.75 0 0 1 16 6.75V9M4 13h16M10 13v2h4v-2"
							/></svg
						>
					{:else if tab.id === 'memory'}
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path d="M3 12a9 9 0 1 0 3-6.7" /><path d="M3 5v5h5" /><path
								d="M12 7v5l3 2"
							/></svg
						>
					{:else}
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path
								d="M9.5 3.5A2.5 2.5 0 0 1 12 1a2.5 2.5 0 0 1 2.5 2.5v.18a2 2 0 0 0 1 1.73l.43.25a2 2 0 0 0 2 0l.15-.08a2.5 2.5 0 0 1 3.41.91l.22.38a2.5 2.5 0 0 1-.91 3.41l-.15.09a2 2 0 0 0-1 1.74v.5a2 2 0 0 0 1 1.74l.15.09a2.5 2.5 0 0 1 .91 3.41l-.22.38a2.5 2.5 0 0 1-3.41.91l-.15-.08a2 2 0 0 0-2 0l-.43.25a2 2 0 0 0-1 1.73v.18A2.5 2.5 0 0 1 12 23a2.5 2.5 0 0 1-2.5-2.5v-.18a2 2 0 0 0-1-1.73l-.43-.25a2 2 0 0 0-2 0l-.15.08a2.5 2.5 0 0 1-3.41-.91l-.22-.38a2.5 2.5 0 0 1 .91-3.41l.15-.09a2 2 0 0 0 1-1.74v-.5a2 2 0 0 0-1-1.74L3.2 9.55a2.5 2.5 0 0 1-.91-3.41l.22-.38a2.5 2.5 0 0 1 3.41-.91l.15.08a2 2 0 0 0 2 0l.43-.25a2 2 0 0 0 1-1.73Z"
							/><circle cx="12" cy="12" r="3.25" /></svg
						>
					{/if}
				</span>
				<span>{tab.label}</span>
			</button>
		{/each}
		<span class="workspace-nav__indicator" aria-hidden="true"></span>
	</div>
</nav>
