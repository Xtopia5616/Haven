<script>
	let { tabs = [], activeTab = 'chat', onNavigate = () => {} } = $props();
</script>

<nav aria-label="工作区导航">
	<div class="workspace-nav md-tabs" role="tablist">
	{#each tabs as tab (tab.id)}
		<button
			type="button"
			id="workspace-tab-{tab.id}"
			class="md-tab"
			class:active={activeTab === tab.id}
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
							d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v7a2.5 2.5 0 0 1-2.5 2.5H11l-4.5 4v-4H6.5A2.5 2.5 0 0 1 4 12.5z"
						/><path d="M9 9h.01M12 9h.01M15 9h.01" /></svg
					>
				{:else if tab.id === 'tools'}
					<svg
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						><path d="m14.7 6.3 3-3a4.2 4.2 0 0 1 0 5.9l-2.1 2.1" /><path
							d="m5.3 17.7 8.6-8.6"
						/><path d="m3 21 3.6-3.6" /><path
							d="m14 14 4.5 4.5a2.1 2.1 0 0 1-3 3L11 17"
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
						><path d="M9 3h6l1 3h3v14H5V6h3z" /><path
							d="M9 3v4h6V3M8 11h8M8 15h5"
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
							d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6 7 7M17 17l1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4"
						/><circle cx="12" cy="12" r="3.5" /><path d="M19 3h2v2" /></svg
					>
				{/if}
			</span>
			<span>{tab.label}</span>
		</button>
	{/each}
	</div>
</nav>
