<script lang="ts">
	import { onMount, tick } from 'svelte';
	import Icon from './Icon.svelte';
	import type { NavigationTab } from './navigationTypes.ts';

	interface Props {
		tabs?: NavigationTab[];
		activeTab?: string;
		onNavigate?: (tabId: string) => void;
		ariaLabel?: string;
		idPrefix?: string;
		panelIdPrefix?: string;
		panelId?: string;
		showIcons?: boolean;
		className?: string;
		isVisible?: boolean;
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
		className = '',
		isVisible = true,
	}: Props = $props();

	let tabsElement: HTMLDivElement | undefined;
	let tabElements = $state<Record<string, HTMLButtonElement>>({});
	let animateIndicator = $state(false);
	let isDraggingTabs = $state(false);
	let canScrollTabsLeft = $state(false);
	let canScrollTabsRight = $state(false);
	let tabDrag: {
		pointerId: number;
		startX: number;
		startY: number;
		startScrollLeft: number;
		moved: boolean;
	} | null = null;
	let suppressDraggedTabClick = false;
	let suppressClickResetTimer: number | undefined;
	let tablistWasVisible = false;
	let previousActiveTab: string | undefined;
	let hasMeasuredIndicator = false;
	let measuredIndicator = $state<{
		x: number;
		y: number;
		width: number;
		height: number;
		visible: boolean;
	}>({
		x: 0,
		y: 0,
		width: 0,
		height: 0,
		visible: false,
	});

	function controlsId(tabId: string): string | undefined {
		if (panelId) return panelId;
		return panelIdPrefix ? `${panelIdPrefix}-${tabId}` : undefined;
	}

	function updateScrollAffordance(): void {
		if (!isVisible || !tabsElement) {
			canScrollTabsLeft = false;
			canScrollTabsRight = false;
			return;
		}

		const maxScrollLeft = tabsElement.scrollWidth - tabsElement.clientWidth;
		canScrollTabsLeft = tabsElement.scrollLeft > 1;
		canScrollTabsRight = maxScrollLeft - tabsElement.scrollLeft > 1;
	}

	function canScrollHorizontally(): boolean {
		if (!tabsElement) return false;
		if (!tabsElement.classList.contains('workspace-secondary-tabs--sidebar')) return false;
		if (getComputedStyle(tabsElement).flexDirection === 'column') return false;
		return tabsElement.scrollWidth > tabsElement.clientWidth + 1;
	}

	function handleTabsWheel(event: WheelEvent): void {
		if (!tabsElement || event.ctrlKey || !canScrollHorizontally()) return;
		if (tabsElement.scrollHeight > tabsElement.clientHeight + 1) return;
		if (Math.abs(event.deltaY) < Math.abs(event.deltaX)) return;
		const maxScrollLeft = tabsElement.scrollWidth - tabsElement.clientWidth;
		if (event.deltaY < 0 && tabsElement.scrollLeft <= 1) return;
		if (event.deltaY > 0 && tabsElement.scrollLeft >= maxScrollLeft - 1) return;

		event.preventDefault();
		tabsElement.scrollLeft += event.deltaY;
	}

	function handleTabsPointerDown(event: PointerEvent): void {
		if (event.pointerType !== 'mouse' || event.button !== 0 || !event.isPrimary) return;
		if (!canScrollHorizontally()) return;
		tabDrag = {
			pointerId: event.pointerId,
			startX: event.clientX,
			startY: event.clientY,
			startScrollLeft: tabsElement?.scrollLeft ?? 0,
			moved: false,
		};
	}

	function handleTabsPointerMove(event: PointerEvent): void {
		if (!tabDrag || event.pointerId !== tabDrag.pointerId || !tabsElement) return;

		const deltaX = event.clientX - tabDrag.startX;
		const deltaY = event.clientY - tabDrag.startY;
		if (!tabDrag.moved) {
			if (Math.hypot(deltaX, deltaY) < 4) return;
			if (Math.abs(deltaX) <= Math.abs(deltaY)) {
				tabDrag = null;
				return;
			}

			tabDrag.moved = true;
			isDraggingTabs = true;
			tabsElement.setPointerCapture?.(event.pointerId);
		}

		event.preventDefault();
		tabsElement.scrollLeft = tabDrag.startScrollLeft - deltaX;
	}

	function handleTabsPointerEnd(event: PointerEvent): void {
		if (!tabDrag || event.pointerId !== tabDrag.pointerId) return;
		const dragged = tabDrag.moved;
		tabDrag = null;
		isDraggingTabs = false;
		if (!dragged) return;

		suppressDraggedTabClick = true;
		if (suppressClickResetTimer !== undefined) {
			window.clearTimeout(suppressClickResetTimer);
		}
		suppressClickResetTimer = window.setTimeout(() => {
			suppressDraggedTabClick = false;
			suppressClickResetTimer = undefined;
		}, 0);
	}

	function handleDraggedTabClick(event: MouseEvent): void {
		if (!suppressDraggedTabClick) return;
		event.preventDefault();
		event.stopImmediatePropagation();
		suppressDraggedTabClick = false;
		if (suppressClickResetTimer !== undefined) {
			window.clearTimeout(suppressClickResetTimer);
			suppressClickResetTimer = undefined;
		}
	}

	function updateMeasuredIndicator(): void {
		updateScrollAffordance();
		if (!isVisible) {
			measuredIndicator.visible = false;
			hasMeasuredIndicator = false;
			animateIndicator = false;
			return;
		}

		const activeElement = tabElements[activeTab];
		if (!tabsElement || !activeElement) {
			measuredIndicator.visible = false;
			hasMeasuredIndicator = false;
			animateIndicator = false;
			return;
		}

		const tabsRect = tabsElement.getBoundingClientRect();
		const tabRect = activeElement.getBoundingClientRect();
		const tabsStyle = getComputedStyle(tabsElement);
		const originLeft = tabsRect.left + tabsElement.clientLeft;
		const originTop = tabsRect.top + tabsElement.clientTop;
		const vertical = tabsStyle.flexDirection === 'column';
		const indicatorHeight =
			Number.parseFloat(tabsStyle.getPropertyValue('--md-comp-tab-indicator-height')) || 3;
		const width = vertical
			? indicatorHeight
			: Number.parseFloat(tabsStyle.getPropertyValue('--md-comp-tab-indicator-min-width')) ||
				24;
		const height = vertical
			? Number.parseFloat(tabsStyle.getPropertyValue('--md-sys-space-xl')) || 20
			: indicatorHeight;

		if (
			tabsRect.width <= 0 ||
			tabsRect.height <= 0 ||
			tabRect.width <= 0 ||
			tabRect.height <= 0
		) {
			measuredIndicator.visible = false;
			hasMeasuredIndicator = false;
			animateIndicator = false;
			return;
		}

		measuredIndicator = {
			x: vertical
				? tabRect.left -
					originLeft +
					(Number.parseFloat(tabsStyle.getPropertyValue('--md-sys-space-sm')) || 8)
				: tabRect.left -
					originLeft +
					tabsElement.scrollLeft +
					(tabRect.width - width) / 2,
			y: vertical
				? tabRect.top - originTop + (tabRect.height - height) / 2
				: tabRect.bottom -
					originTop -
					(Number.parseFloat(
						tabsStyle.getPropertyValue('--md-comp-tab-indicator-bottom'),
					) || 3) -
					height,
			width,
			height,
			visible: true,
		};
		hasMeasuredIndicator = true;
	}

	$effect(() => {
		const nextActiveTab = activeTab;
		tabs;
		const visible = isVisible;
		if (!visible) {
			measuredIndicator.visible = false;
			hasMeasuredIndicator = false;
			animateIndicator = false;
			isDraggingTabs = false;
			tabDrag = null;
			canScrollTabsLeft = false;
			canScrollTabsRight = false;
			previousActiveTab = nextActiveTab;
			tablistWasVisible = false;
			return;
		}

		// Only a selection change within an already visible tablist should move
		// the indicator. Initial layout and keep-alive re-entry must snap to the
		// freshly measured position instead of animating from stale coordinates.
		const shouldAnimate =
			tablistWasVisible &&
			hasMeasuredIndicator &&
			previousActiveTab !== undefined &&
			previousActiveTab !== nextActiveTab;
		previousActiveTab = nextActiveTab;
		tablistWasVisible = true;

		let cancelled = false;
		let cancelPendingMeasurement: (() => void) | undefined;
		void tick().then(() => {
			if (cancelled) return;
			const measure = () => {
				cancelPendingMeasurement = undefined;
				if (!cancelled) updateMeasuredIndicator();
			};
			if (typeof requestAnimationFrame === 'function') {
				if (shouldAnimate) {
					animateIndicator = true;
					const prepareFrame = requestAnimationFrame(() => {
						if (cancelled) return;
						const measureFrame = requestAnimationFrame(measure);
						cancelPendingMeasurement = () => cancelAnimationFrame(measureFrame);
					});
					cancelPendingMeasurement = () => cancelAnimationFrame(prepareFrame);
				} else {
					animateIndicator = false;
					const frame = requestAnimationFrame(measure);
					cancelPendingMeasurement = () => cancelAnimationFrame(frame);
				}
			} else {
				animateIndicator = false;
				const timeout = window.setTimeout(measure, 0);
				cancelPendingMeasurement = () => window.clearTimeout(timeout);
			}
		});
		return () => {
			cancelled = true;
			cancelPendingMeasurement?.();
		};
	});

	function finishIndicatorTransition(event: TransitionEvent): void {
		if (event.target !== event.currentTarget || event.propertyName !== 'transform') return;
		animateIndicator = false;
	}

	onMount(() => {
		const handleResize = () => updateMeasuredIndicator();
		window.addEventListener('resize', handleResize);
		tabsElement?.addEventListener('wheel', handleTabsWheel, { passive: false });
		tabsElement?.addEventListener('pointerdown', handleTabsPointerDown);
		tabsElement?.addEventListener('click', handleDraggedTabClick, true);
		window.addEventListener('pointermove', handleTabsPointerMove);
		window.addEventListener('pointerup', handleTabsPointerEnd);
		window.addEventListener('pointercancel', handleTabsPointerEnd);
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
			tabsElement?.removeEventListener('wheel', handleTabsWheel);
			tabsElement?.removeEventListener('pointerdown', handleTabsPointerDown);
			tabsElement?.removeEventListener('click', handleDraggedTabClick, true);
			window.removeEventListener('pointermove', handleTabsPointerMove);
			window.removeEventListener('pointerup', handleTabsPointerEnd);
			window.removeEventListener('pointercancel', handleTabsPointerEnd);
			if (suppressClickResetTimer !== undefined) {
				window.clearTimeout(suppressClickResetTimer);
			}
			resizeObserver?.disconnect();
		};
	});
</script>

<div
	bind:this={tabsElement}
	class="md-tabs {className}"
	class:md-tabs--indicator-ready={measuredIndicator.visible && isVisible}
	class:md-tabs--indicator-animating={animateIndicator}
	class:md-tabs--scrollable={canScrollTabsLeft || canScrollTabsRight}
	class:md-tabs--dragging={isDraggingTabs}
	class:md-tabs--scroll-hint-left={canScrollTabsLeft}
	class:md-tabs--scroll-hint-right={canScrollTabsRight}
	style={`--md-tab-indicator-x: ${measuredIndicator.x}px; --md-tab-indicator-y: ${measuredIndicator.y}px; --md-tab-indicator-width: ${measuredIndicator.width}px; --md-tab-indicator-height: ${measuredIndicator.height}px;`}
	role="tablist"
	aria-label={ariaLabel}
	onscroll={updateScrollAffordance}
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
	<span
		class="md-tabs__indicator"
		aria-hidden="true"
		ontransitionend={finishIndicatorTransition}
	></span>
</div>

<style>
	.md-tabs {
		position: relative;
	}

	.md-tabs .md-tabs__indicator {
		position: absolute;
		z-index: 1;
		left: 0;
		top: 0;
		width: var(--md-tab-indicator-width);
		height: var(--md-tab-indicator-height);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		transform: translate(var(--md-tab-indicator-x), var(--md-tab-indicator-y));
		opacity: 0;
		transition: none;
		pointer-events: none;
	}

	.md-tabs--indicator-animating .md-tabs__indicator {
		transition:
			transform var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-emphasized),
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}

	.md-tabs--indicator-ready .md-tabs__indicator {
		opacity: 1;
	}

	.md-tab small {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
	}

</style>
