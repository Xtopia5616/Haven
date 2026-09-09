<script>
	import { onMount, tick } from 'svelte';
	import Icon from './Icon.svelte';

	/**
	 * Material Tabs — the shared tablist primitive for workspace and page tabs.
	 * @prop {Array<{id: string, label: string, hint?: string, icon?: string}>} tabs
	 * @prop {string} activeTab
	 * @prop {function} onNavigate
	 * @prop {string} ariaLabel
	 * @prop {string} idPrefix
	 * @prop {string} panelIdPrefix — set to an empty string when no tabpanel is owned here
	 * @prop {string | undefined} panelId — explicit shared panel id
	 * @prop {boolean} showIcons
	 * @prop {'css'|'measured'} indicator
	 * @prop {string} className
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
	} = $props();

	/** @type {HTMLDivElement | undefined} */
	let tabsElement;
	/** @type {Record<string, HTMLButtonElement>} */
	let tabElements = $state({});
	let measuredIndicator = $state({ x: 0, width: 24, visible: false });

	/** @param {string} tabId */
	function controlsId(tabId) {
		if (panelId) return panelId;
		return panelIdPrefix ? `${panelIdPrefix}-${tabId}` : undefined;
	}

	function updateMeasuredIndicator() {
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
		/** @type {ResizeObserver | undefined} */
		let resizeObserver;
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
