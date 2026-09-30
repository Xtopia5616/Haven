<script lang="ts">
	import { onMount, tick } from 'svelte';
	import Icon from './Icon.svelte';

	type TabItem = { id: string; label: string; hint?: string; icon?: string };

	interface Props {
		tabs?: TabItem[];
		activeTab?: string;
		onNavigate?: (tabId: string) => void;
		ariaLabel?: string;
		idPrefix?: string;
		panelIdPrefix?: string;
		panelId?: string;
		showIcons?: boolean;
		indicator?: 'css' | 'measured';
		className?: string;
	}

	/**
	 * Material Tabs — the shared tablist primitive for workspace and page tabs.
	 */
	let {
		tabs = [],
		activeTab = '',
		onNavigate = () => {},
		ariaLabel = '页签',
		idPrefix = 'tab',
		panelIdPrefix = 'tabpanel',
		panelId = undefined,
		showIcons = false,
		indicator = 'css',
		className = '',
	}: Props = $props();

	let tabsElement: HTMLDivElement | undefined;
	let tabElements = $state<Record<string, HTMLButtonElement>>({});
	let measuredIndicator = $state<{ x: number; width: number; visible: boolean }>({
		x: 0,
		width: 24,
		visible: false,
	});

	function controlsId(tabId: string): string | undefined {
		if (panelId) return panelId;
		return panelIdPrefix ? `${panelIdPrefix}-${tabId}` : undefined;
	}

	function updateMeasuredIndicator(): void {
		if (indicator !== 'measured') return;
		const activeElement = tabElements[activeTab];
		if (!tabsElement || !activeElement) return;

		const tabsRect = tabsElement.getBoundingClientRect();
		const tabRect = activeElement.getBoundingClientRect();
		const width =
			Number.parseFloat(
				getComputedStyle(tabsElement).getPropertyValue('--md-comp-tab-indicator-min-width'),
			) || 24;

		if (tabRect.width <= 0) {
			measuredIndicator.visible = false;
			return;
		}

		measuredIndicator = {
			x: tabRect.left - tabsRect.left + (tabRect.width - width) / 2,
			width,
			visible: true,
		};
	}

	$effect(() => {
		activeTab;
		tabs;
		indicator;
		if (indicator === 'measured') void tick().then(updateMeasuredIndicator);
	});

	onMount(() => {
		if (indicator !== 'measured') return;
		const handleResize = () => updateMeasuredIndicator();
		window.addEventListener('resize', handleResize);
		let resizeObserver: ResizeObserver | undefined;
		if (typeof ResizeObserver !== 'undefined' && tabsElement) {
			const observer = new ResizeObserver(handleResize);
			resizeObserver = observer;
			observer.observe(tabsElement);
			Object.values(tabElements).forEach((element) => observer.observe(element));
		}
		updateMeasuredIndicator();

		return () => {
			window.removeEventListener('resize', handleResize);
			resizeObserver?.disconnect();
		};
	});
</script>

<div
	bind:this={tabsElement}
	class="md-tabs {className}"
	class:md-tabs--measured={indicator === 'measured'}
	class:md-tabs--indicator-ready={measuredIndicator.visible}
	style={`--md-tab-indicator-x: ${measuredIndicator.x}px; --md-tab-indicator-width: ${measuredIndicator.width}px;`}
	role="tablist"
	aria-label={ariaLabel}
>
	{#each tabs as tab (tab.id)}
		<button
			type="button"
			id={`${idPrefix}-${tab.id}`}
			class="md-tab"
			class:active={activeTab === tab.id}
			bind:this={tabElements[tab.id]}
			aria-selected={activeTab === tab.id}
			aria-controls={controlsId(tab.id)}
			tabindex={activeTab === tab.id ? 0 : -1}
			role="tab"
			onclick={() => onNavigate(tab.id)}
		>
			{#if showIcons}
				<span class="md-tab__icon" aria-hidden="true">
					<Icon name={tab.icon || tab.id || 'settings'} size={17} />
				</span>
			{/if}
			<span>{tab.label}</span>
			{#if tab.hint}<small>{tab.hint}</small>{/if}
		</button>
	{/each}
	{#if indicator === 'measured'}
		<span class="workspace-nav__indicator" aria-hidden="true"></span>
	{/if}
</div>

<style>
	.md-tabs--measured {
		position: relative;
	}

	.md-tabs--measured .workspace-nav__indicator {
		position: absolute;
		bottom: 0;
		left: 0;
		width: var(--md-tab-indicator-width);
		height: var(--md-comp-tab-indicator-height);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		transform: translateX(var(--md-tab-indicator-x));
		opacity: 0;
		transition:
			transform var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-emphasized),
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
		pointer-events: none;
	}

	.md-tabs--measured.md-tabs--indicator-ready .workspace-nav__indicator {
		opacity: 1;
	}

	.md-tab small {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
	}

	@media (prefers-reduced-motion: reduce) {
		.md-tabs--measured .workspace-nav__indicator {
			transition: none;
		}
	}
</style>
