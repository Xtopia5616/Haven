<script>
	import { onMount, tick } from 'svelte';

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
					{#if (tab.icon || tab.id) === 'chat'}
						<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5.5 4.5h10A3.5 3.5 0 0 1 19 8v4.25a3.5 3.5 0 0 1-3.5 3.5H11l-5.5 4v-4.04a3.5 3.5 0 0 1-3.5-3.46V8a3.5 3.5 0 0 1 3.5-3.5Z" /><path d="M7 9h7M7 12h4" /></svg>
					{:else if (tab.icon || tab.id) === 'tasks'}
						<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="3.5" width="16" height="17" rx="3" /><path d="M8 8h.01M11.5 8H16M8 12h.01M11.5 12H16M8 16h.01M11.5 16H14" /></svg>
					{:else if (tab.icon || tab.id) === 'tools'}
						<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 9h16v10.5H4zM8 9V6.75A1.75 1.75 0 0 1 9.75 5h4.5A1.75 1.75 0 0 1 16 6.75V9M4 13h16M10 13v2h4v-2" /></svg>
					{:else if (tab.icon || tab.id) === 'memory'}
						<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 3-6.7" /><path d="M3 5v5h5" /><path d="M12 7v5l3 2" /></svg>
					{:else}
						<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9.5 3.5A2.5 2.5 0 0 1 12 1a2.5 2.5 0 0 1 2.5 2.5v.18a2 2 0 0 0 1 1.73l.43.25a2 2 0 0 0 2 0l.15-.08a2.5 2.5 0 0 1 3.41.91l.22.38a2.5 2.5 0 0 1-.91 3.41l-.15.09a2 2 0 0 0-1 1.74v.5a2 2 0 0 0 1 1.74l.15.09a2.5 2.5 0 0 1 .91 3.41l-.22.38a2.5 2.5 0 0 1-3.41.91l-.15-.08a2 2 0 0 0-2 0l-.43.25a2 2 0 0 0-1 1.73v.18A2.5 2.5 0 0 1 12 23a2.5 2.5 0 0 1-2.5-2.5v-.18a2 2 0 0 0-1-1.73l-.43-.25a2 2 0 0 0-2 0l-.15.08a2.5 2.5 0 0 1-3.41-.91l-.22-.38a2.5 2.5 0 0 1 .91-3.41l.15-.09a2 2 0 0 0 1-1.74v-.5a2 2 0 0 0-1-1.74L3.2 9.55a2.5 2.5 0 0 1-.91-3.41l.22-.38a2.5 2.5 0 0 1 3.41-.91l.15.08a2.5 2.5 0 0 0 2 0l.43-.25a2.5 2.5 0 0 0 1-1.73Z" /><circle cx="12" cy="12" r="3.25" /></svg>
					{/if}
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
