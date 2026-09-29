<script lang="ts">
	import Icon from './Icon.svelte';

	interface WorkspaceTab {
		id: string;
		label: string;
		hint?: string;
		icon?: string;
	}

	interface Props {
		tabs?: WorkspaceTab[];
		activeTab?: string;
		onNavigate?: (tabId: string) => void;
	}

	let { tabs = [], activeTab = 'chat', onNavigate = () => {} }: Props = $props();
</script>

<nav class="landscape-workspace-nav" aria-label="工作区">
	{#each tabs as tab (tab.id)}
		<button
			type="button"
			class="workspace-link"
			class:active={activeTab === tab.id}
			aria-current={activeTab === tab.id ? 'page' : undefined}
			aria-label={tab.label}
			title={tab.hint ? `${tab.label} · ${tab.hint}` : tab.label}
			onclick={() => onNavigate(tab.id)}
		>
			<span class="workspace-link__icon" aria-hidden="true">
				<Icon name={tab.icon || tab.id || 'settings'} size={19} />
			</span>
			<span class="workspace-link__copy">
				<span class="workspace-link__label">{tab.label}</span>
				{#if tab.hint}<span class="workspace-link__hint">{tab.hint}</span>{/if}
			</span>
			{#if activeTab === tab.id}<span class="workspace-link__current" aria-hidden="true"
				></span>{/if}
		</button>
	{/each}
</nav>

<style>
	.landscape-workspace-nav {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.workspace-link {
		position: relative;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		width: 100%;
		min-height: 54px;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid transparent;
		border-radius: var(--md-sys-shape-medium);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font: inherit;
		text-align: left;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			transform var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.workspace-link:hover {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
	}
	.workspace-link:active {
		transform: scale(0.985);
	}
	.workspace-link:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: 2px;
	}
	.workspace-link.active {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-primary) 18%,
			var(--md-sys-color-outline-variant)
		);
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 74%,
			var(--md-sys-color-surface-container-low) 26%
		);
		color: var(--md-sys-color-on-primary-container);
	}
	.workspace-link__icon {
		display: grid;
		place-items: center;
		width: 32px;
		height: 32px;
		flex: 0 0 32px;
		border-radius: var(--md-sys-shape-small);
		color: inherit;
	}
	.workspace-link.active .workspace-link__icon {
		background: color-mix(in srgb, var(--md-sys-color-primary) 12%, transparent);
		color: var(--md-sys-color-primary);
	}
	.workspace-link__copy {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
		gap: 2px;
	}
	.workspace-link__label {
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.workspace-link__hint {
		overflow: hidden;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.workspace-link__current {
		width: 5px;
		height: 5px;
		flex: 0 0 5px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
	}
</style>
