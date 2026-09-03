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
							><path
								d="M9.5 4.5a3 3 0 0 0-5.2 2A3.25 3.25 0 0 0 3 12a3.25 3.25 0 0 0 1.3 2.6A3 3 0 0 0 9.5 19M14.5 4.5a3 3 0 0 1 5.2 2A3.25 3.25 0 0 1 21 12a3.25 3.25 0 0 1-1.3 2.6A3 3 0 0 1 14.5 19"
							/><path
								d="M9.5 4.5v15M14.5 4.5v15M5.5 8.5h4M14.5 8.5h4M5 13h4.5M14.5 13H19"
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
	</div>
</nav>
