<script lang="ts">
	import { tick, type ComponentProps } from 'svelte';
	import { fade } from 'svelte/transition';
	import SessionRail from './SessionRail.svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';
	import { sessionSidebarLayoutStore } from './sessionSidebarLayoutStore.ts';

	let props: Omit<ComponentProps<typeof SessionRail>, 'onCollapse'> = $props();
	const collapsed = $derived($sessionSidebarLayoutStore.collapsed);
	const width = $derived($sessionSidebarLayoutStore.width);
	let dragging = $state(false);
	let sidebar: HTMLDivElement;
	let dragStart = 0;
	let startWidth = 240;
	const minimumWidth = 200;
	const maximumWidth = 360;

	async function toggleCollapsed() {
		sessionSidebarLayoutStore.update((layout) => ({ ...layout, collapsed: !layout.collapsed }));
		await tick();
		sidebar.querySelector<HTMLButtonElement>('[aria-expanded]')?.focus();
	}

	function resize(next: number) {
		sessionSidebarLayoutStore.update((layout) => ({
			...layout,
			width: Math.min(maximumWidth, Math.max(minimumWidth, next)),
		}));
	}

	function startResize(event: PointerEvent) {
		if (event.button !== 0) return;
		const handle = event.currentTarget as HTMLElement;
		handle.setPointerCapture(event.pointerId);
		dragStart = event.clientX;
		startWidth = sidebar.getBoundingClientRect().width;
		dragging = true;
	}

	function resizeWithKeyboard(event: KeyboardEvent) {
		if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
		event.preventDefault();
		resize(
			event.key === 'Home'
				? minimumWidth
				: event.key === 'End'
					? maximumWidth
					: width + (event.key === 'ArrowRight' ? 16 : -16),
		);
	}
</script>

<div
	class="session-sidebar"
	class:collapsed
	class:dragging
	bind:this={sidebar}
	style={`--session-sidebar-width: ${width}px`}
>
	{#if collapsed}
		<div class="session-sidebar__compact" in:fade={{ duration: 160 }}>
			<MaterialIconButton
				label="展开会话栏"
				title="展开会话栏"
				icon="chevronRight"
				variant="ghost"
				ariaExpanded={false}
				onclick={toggleCollapsed}
			/>
			<MaterialIconButton
				label="新建会话"
				title="新建会话"
				icon="plus"
				variant="ghost"
				onclick={() => props.onNew?.()}
			/>
		</div>
	{:else}
		<SessionRail {...props} onCollapse={toggleCollapsed} />
		<!-- A focusable separator implements the WAI-ARIA window splitter keyboard pattern. -->
		<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
		<div
			class="session-sidebar__resize"
			role="separator"
			tabindex="0"
			aria-label="调整会话栏宽度"
			aria-orientation="vertical"
			aria-valuemin={minimumWidth}
			aria-valuemax={maximumWidth}
			aria-valuenow={width}
			title="拖动调整宽度，双击恢复默认；方向键调宽，Home / End 调至最小 / 最大"
			onpointerdown={startResize}
			onpointermove={(event) => {
				if (dragging) resize(startWidth + event.clientX - dragStart);
			}}
			onpointerup={() => (dragging = false)}
			onpointercancel={() => (dragging = false)}
			onlostpointercapture={() => (dragging = false)}
			onkeydown={resizeWithKeyboard}
			ondblclick={() => resize(240)}
		></div>
	{/if}
</div>

<style>
	.session-sidebar {
		position: relative;
		display: flex;
		width: var(--session-sidebar-width);
		min-height: 0;
		background: var(--md-sys-color-surface-container-low);
		transition: width var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard);
	}
	.session-sidebar.collapsed {
		width: 48px;
	}
	.session-sidebar__compact {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding-top: var(--md-sys-space-lg);
	}
	.session-sidebar__resize {
		position: absolute;
		right: 0;
		top: 0;
		bottom: 0;
		width: 6px;
		cursor: col-resize;
		touch-action: none;
	}
	.session-sidebar__resize:hover,
	.session-sidebar__resize:focus-visible,
	.dragging .session-sidebar__resize {
		background: var(--md-sys-color-primary);
	}
	.dragging {
		user-select: none;
		transition: none;
	}
</style>
