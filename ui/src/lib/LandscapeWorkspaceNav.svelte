<script lang="ts">
	import { onMount, tick } from 'svelte';
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
	let navElement: HTMLElement | undefined;
	let linkElements = $state<Record<string, HTMLButtonElement>>({});
	let measuredIndicator = $state({ x: 0, y: 0, visible: false });

	function updateMeasuredIndicator(): void {
		const activeElement = linkElements[activeTab];
		if (!navElement || !activeElement) {
			measuredIndicator.visible = false;
			return;
		}

		const navRect = navElement.getBoundingClientRect();
		const linkRect = activeElement.getBoundingClientRect();
		const navStyle = getComputedStyle(navElement);
		const indicatorHeight =
			Number.parseFloat(navStyle.getPropertyValue('--md-sys-space-xl')) || 20;
		const indicatorInset =
			Number.parseFloat(navStyle.getPropertyValue('--md-sys-space-xs')) || 4;

		if (
			navRect.width <= 0 ||
			navRect.height <= 0 ||
			linkRect.width <= 0 ||
			linkRect.height <= 0
		) {
			measuredIndicator.visible = false;
			return;
		}

		measuredIndicator = {
			x: linkRect.left - navRect.left + indicatorInset,
			y: linkRect.top - navRect.top + (linkRect.height - indicatorHeight) / 2,
			visible: true,
		};
	}

	$effect(() => {
		activeTab;
		tabs;
		let cancelled = false;
		let cancelPendingMeasurement: (() => void) | undefined;

		void tick().then(() => {
			if (cancelled) return;
			const measure = () => {
				cancelPendingMeasurement = undefined;
				if (!cancelled) updateMeasuredIndicator();
			};
			if (typeof requestAnimationFrame === 'function') {
				const frame = requestAnimationFrame(measure);
				cancelPendingMeasurement = () => cancelAnimationFrame(frame);
			} else {
				const timeout = window.setTimeout(measure, 0);
				cancelPendingMeasurement = () => window.clearTimeout(timeout);
			}
		});

		return () => {
			cancelled = true;
			cancelPendingMeasurement?.();
		};
	});

	onMount(() => {
		const handleResize = () => updateMeasuredIndicator();
		window.addEventListener('resize', handleResize);
		let resizeObserver: ResizeObserver | undefined;
		if (typeof ResizeObserver !== 'undefined' && navElement) {
			const observer = new ResizeObserver(handleResize);
			resizeObserver = observer;
			observer.observe(navElement);
			Object.values(linkElements).forEach((element) => observer.observe(element));
		}
		updateMeasuredIndicator();

		return () => {
			window.removeEventListener('resize', handleResize);
			resizeObserver?.disconnect();
		};
	});
</script>

<nav
	bind:this={navElement}
	class="landscape-workspace-nav"
	class:landscape-workspace-nav--indicator-ready={measuredIndicator.visible}
	style={`--workspace-indicator-x: ${measuredIndicator.x}px; --workspace-indicator-y: ${measuredIndicator.y}px;`}
	aria-label="工作区"
>
	{#each tabs as tab (tab.id)}
		<button
			type="button"
			class="workspace-link"
			class:active={activeTab === tab.id}
			bind:this={linkElements[tab.id]}
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
		</button>
	{/each}
	<span class="landscape-workspace-nav__indicator" aria-hidden="true"></span>
</nav>

<style>
	.landscape-workspace-nav {
		position: relative;
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
			transform var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			width var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			padding var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
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
		border-color: transparent;
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.landscape-workspace-nav__indicator {
		position: absolute;
		z-index: 1;
		left: 0;
		top: 0;
		width: var(--md-comp-tab-indicator-height);
		height: var(--md-sys-space-xl);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		transform: translate(var(--workspace-indicator-x), var(--workspace-indicator-y));
		opacity: 0;
		transition:
			transform var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-emphasized),
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
		pointer-events: none;
	}
	.landscape-workspace-nav--indicator-ready .landscape-workspace-nav__indicator {
		opacity: 1;
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
</style>
