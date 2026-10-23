<script lang="ts">
	import type { Snippet } from 'svelte';
	import Logo from './Logo.svelte';
	import RecordingIndicator from './RecordingIndicator.svelte';
	import NotificationToast from './NotificationToast.svelte';
	import WorkspaceNav from './WorkspaceNav.svelte';
	import MaterialButton from './MaterialButton.svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';
	import MaterialBadge from './MaterialBadge.svelte';
	import Icon from './Icon.svelte';
	import GlobalContextMenu from './GlobalContextMenu.svelte';
	import LandscapeWorkspaceNav from './LandscapeWorkspaceNav.svelte';
	import { dragScroll } from '$lib/dragScroll.ts';
	import type { RecordingOverlayState } from '$lib/recordingOverlayController.ts';
	import type { NavigationTabsProps } from '$lib/navigationTypes.ts';
	import type { ThemeMode } from '$lib/themeStore.ts';

	interface Props extends NavigationTabsProps {
		navigationTabs?: NavigationTabsProps['tabs'];
		navigationActiveTab?: NavigationTabsProps['activeTab'];
		theme?: ThemeMode;
		onToggleTheme?: () => void;
		onRefreshApp?: () => void;
		overlay?: RecordingOverlayState;
		duration?: number;
		onCancelRecording?: (() => Promise<void>) | null;
		status?: Snippet;
		content?: Snippet;
	}

	/**
	 * The shell owns chrome and layout only. Domain state stays in the route and
	 * reaches the shell through named snippets or explicit props.
	 */
	let {
		activeTab = 'chat',
		tabs = [],
		navigationTabs = undefined,
		navigationActiveTab = undefined,
		theme = 'dark',
		onToggleTheme = () => {},
		onRefreshApp = () => {},
		onNavigate = () => {},
		overlay = {
			visible: false,
			isRecording: false,
			processing: false,
			recordingId: null,
			startedAt: null,
			vadState: 'silent',
		},
		duration = 0,
		onCancelRecording = null,
		status,
		content,
	}: Props = $props();

	const isDevelopmentBuild = import.meta.env.DEV;
</script>

{#snippet themeControl()}
	<MaterialIconButton
		className="rail-theme-button"
		size="toolbar"
		variant="ghost"
		onclick={() => onToggleTheme?.()}
		label={theme === 'dark' ? '切换到亮色模式' : '切换到暗色模式'}
		title={theme === 'dark' ? '切换到亮色模式' : '切换到暗色模式'}
	>
		{#snippet children()}
			{#if theme === 'dark'}
				<Icon name="sun" size={18} className="theme-icon" />
			{:else}
				<Icon name="moon" size={18} className="theme-icon" />
			{/if}
		{/snippet}
	</MaterialIconButton>
{/snippet}
{#snippet shellControls()}
	{@render status?.()}
	{@render themeControl()}
{/snippet}

<div class="app-shell responsive-layout-transition">
	<header class="titlebar md-toolbar">
		<div class="titlebar-left">
			<MaterialButton
				variant="text"
				className="titlebar-logo"
				ariaLabel="刷新应用状态"
				title="刷新应用状态"
				onclick={() => onRefreshApp?.()}
			>
				{#snippet children()}
					<Logo size={22} withText={true} />
				{/snippet}
			</MaterialButton>
			{#if isDevelopmentBuild}
				<MaterialBadge text="dev" variant="warning" title="此窗口连接 Haven 开发环境" />
			{/if}
		</div>
		<div class="titlebar-right">
			{@render shellControls()}
		</div>
	</header>

	<aside class="workspace-rail" aria-label="Haven 主导航">
		<button
			type="button"
			class="rail-brand"
			aria-label="刷新应用状态"
			title="刷新应用状态"
			onclick={() => onRefreshApp?.()}
		>
			<Logo size={30} withText={true} />
		</button>
		<div class="rail-navigation">
			<LandscapeWorkspaceNav
				tabs={navigationTabs ?? tabs}
				activeTab={navigationActiveTab ?? activeTab}
				{onNavigate}
			/>
		</div>
		<div class="rail-footer">
			{@render shellControls()}
		</div>
	</aside>

	<div class="compact-workspace-nav">
		<WorkspaceNav
			tabs={navigationTabs ?? tabs}
			activeTab={navigationActiveTab ?? activeTab}
			{onNavigate}
		/>
	</div>

	<main
		class="content"
		class:content--chat={activeTab === 'chat'}
		use:dragScroll={{ axis: 'y', preserveTextSelection: true }}
	>
		{@render content?.()}
	</main>

	<RecordingIndicator
		isRecording={overlay.isRecording}
		processing={overlay.processing}
		{duration}
		vadState={overlay.vadState}
		onCancel={onCancelRecording}
	/>
	<NotificationToast />
	<GlobalContextMenu />
</div>

<style>
	.app-shell {
		--workspace-rail-width: 0px;
		display: flex;
		flex-direction: column;
		grid-template-columns: var(--workspace-rail-width) minmax(0, 1fr);
		height: 100vh;
		min-width: 0;
		min-height: 0;
		background: var(--md-sys-color-background);
		color: var(--md-sys-color-on-surface);
	}
	.titlebar {
		position: relative;
		z-index: 0;
		height: var(--md-comp-titlebar-height);
		background: var(--md-sys-color-titlebar);
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0 var(--md-sys-space-xl);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		flex-shrink: 0;
		-webkit-app-region: drag;
	}
	.titlebar-left,
	.titlebar-right {
		display: flex;
		align-items: center;
	}
	.titlebar-left {
		gap: var(--md-sys-space-sm);
	}
	.titlebar-left :global(.md-badge) {
		-webkit-app-region: no-drag;
	}
	.workspace-rail {
		display: none;
	}
	.compact-workspace-nav {
		flex: 0 0 auto;
	}
	:global(.md-btn.titlebar-logo) {
		display: inline-flex;
		align-items: center;
		height: var(--md-comp-toolbar-height);
		padding: 0 var(--md-sys-space-xs);
		border: 0;
		border-radius: var(--md-sys-shape-medium);
		background: transparent;
		color: inherit;
		cursor: pointer;
		-webkit-app-region: no-drag;
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			transform var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-emphasized);
	}
	:global(.md-btn.titlebar-logo:hover) {
		background: color-mix(in srgb, var(--md-sys-color-primary) 10%, transparent);
	}
	:global(.md-btn.titlebar-logo:active) {
		transform: scale(0.97);
	}
	.titlebar-right {
		gap: var(--md-sys-space-sm);
		-webkit-app-region: no-drag;
	}
	:global(.theme-icon) {
		display: block;
		width: 18px;
		height: 18px;
	}
	.content {
		--workspace-surface-bottom-gap: var(--md-sys-content-gutter);
		position: relative;
		flex: 1;
		min-width: 0;
		min-height: 0;
		overflow-y: auto;
		overflow-x: clip;
		overscroll-behavior-x: none;
		touch-action: pan-y;
		padding: var(--md-sys-content-gutter);
		transition: padding var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		background: var(--md-sys-color-surface);
		background-image: linear-gradient(
			180deg,
			color-mix(in srgb, var(--md-sys-color-primary) 3%, transparent),
			transparent 240px
		);
	}
	.content:not(.content--chat) {
		padding-bottom: var(--workspace-surface-bottom-gap);
		overscroll-behavior-y: none;
	}
	@media screen and (min-width: 840px) {
		.content:not(.content--chat) {
			--workspace-surface-bottom-gap: var(--md-sys-space-xl);
		}
	}
	.content--chat {
		overflow: hidden;
		overflow-x: clip;
		padding: 0;
		display: flex;
		flex-direction: column;
	}
	:global(.content.drag-scroll--active) {
		cursor: grabbing;
	}
	:global(.page-shell) {
		width: 100%;
		margin: 0 auto;
	}
	/* `display:flex` on the chat panel used to outrank the native hidden
	 * attribute after another keep-alive view had been visited. That made
	 * inactive views participate in the chat layout and was the source of
	 * intermittent blank space/overlap after switching tabs. */
	:global(.tab-panel[hidden]) {
		display: none !important;
	}
	:global(.content--chat .tab-panel:not([hidden])) {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}
	/* Secondary workspaces use WorkspaceSurface for their shared frame and
	 * entry motion. Keeping chat outside that component preserves its full-bleed
	 * chat layout. */
	:global(.content:not(.content--chat) .tab-panel:not([hidden])) {
		display: flex;
		flex-direction: column;
		min-height: 100%;
	}
	:global(.content:not(.content--chat) .page-shell) {
		display: flex;
		flex: 1;
		flex-direction: column;
		max-width: clamp(640px, 92vw, var(--md-sys-content-max-width));
		min-width: 0;
	}
	:global(.content:not(.content--chat) .page-shell--loading) {
		display: flex;
		flex-direction: column;
		min-height: 100%;
	}
	:global(.content--chat .page-shell) {
		flex: 1;
		width: 100%;
		min-width: 0;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}
	:global(.chat-tab-enter) {
		animation: haven-chat-tab-enter var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard) both;
	}
	:global(.tab-panel--leaving) {
		position: absolute;
		inset: 0;
		z-index: 1;
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		pointer-events: none;
		animation: haven-tab-panel-exit var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard) both;
	}
	:global(.content:not(.content--chat) .tab-panel--leaving .page-shell) {
		display: flex;
		flex: 1;
		flex-direction: column;
		width: 100%;
		max-width: none;
		min-height: 0;
	}
	@keyframes haven-chat-tab-enter {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}
	@keyframes haven-tab-panel-exit {
		from {
			opacity: 1;
		}
		to {
			opacity: 0;
		}
	}
	@media (max-width: 640px) {
		.titlebar {
			padding: 0 var(--md-sys-space-md);
		}
	}

	@media screen and (min-width: 600px) {
		.app-shell {
			--workspace-rail-width: clamp(120px, 12vw, 152px);
			--md-comp-titlebar-height: var(--md-sys-space-4xl);
			display: grid;
			grid-template-columns: var(--workspace-rail-width) minmax(0, 1fr);
			grid-template-rows: var(--md-comp-titlebar-height) minmax(0, 1fr);
			background: var(--md-sys-color-background);
		}
		.titlebar {
			grid-column: 2;
			grid-row: 1;
			min-width: 0;
			padding: 0 var(--md-sys-space-xl);
			background: var(--md-sys-color-titlebar);
			border-bottom-color: var(--md-sys-color-outline-variant);
		}
		.titlebar-left {
			display: flex;
		}
		:global(.md-btn.titlebar-logo) {
			display: none;
		}
		.titlebar-right {
			display: none;
		}
		.workspace-rail {
			--rail-control-size: 36px;
			grid-column: 1;
			grid-row: 1 / span 2;
			display: flex;
			flex-direction: column;
			gap: var(--md-sys-space-xl);
			min-width: 0;
			min-height: 0;
			padding: var(--md-sys-space-lg) var(--md-sys-space-sm);
			border-right: 1px solid var(--md-sys-color-outline-variant);
			background: var(--md-sys-color-surface-container-low);
		}
		.rail-brand {
			display: flex;
			align-items: center;
			width: 100%;
			min-height: 44px;
			flex-shrink: 0;
			padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
			border: 0;
			border-radius: var(--md-sys-shape-medium);
			background: transparent;
			cursor: pointer;
			-webkit-app-region: no-drag;
			transition: background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
		}
		.rail-brand:hover {
			background: var(--md-sys-color-surface-container-high);
		}
		.rail-navigation {
			flex: 0 1 auto;
			min-height: 0;
			overflow-y: auto;
		}
		.workspace-rail :global(.workspace-link) {
			flex-shrink: 0;
			gap: var(--md-sys-space-xs);
			min-height: 48px;
			padding: var(--md-sys-space-sm);
		}
		.workspace-rail :global(.workspace-link__label) {
			font-size: var(--md-sys-typescale-body-small-size);
			white-space: nowrap;
		}
		.workspace-rail :global(.workspace-link__hint) {
			display: none;
		}
		.rail-footer {
			display: flex;
			align-items: center;
			gap: var(--md-sys-space-sm);
			flex-shrink: 0;
			margin-top: auto;
			padding-top: var(--md-sys-space-md);
			border-top: 1px solid var(--md-sys-color-outline-variant);
		}
		.rail-footer :global(.status-switch) {
			display: flex;
			align-items: center;
			flex: 1 1 auto;
			gap: var(--md-sys-space-xs);
			min-width: 0;
		}
		.rail-footer :global(.status-chip) {
			flex: 1 1 auto;
			justify-content: flex-start;
			min-width: 0;
			width: auto;
			height: var(--rail-control-size);
			padding-inline: var(--md-sys-space-sm);
		}
		.rail-footer :global(.status-text) {
			min-width: 0;
			overflow: hidden;
			text-overflow: ellipsis;
			white-space: nowrap;
		}
		.rail-footer :global(.task-action) {
			flex-shrink: 0;
		}
		.rail-footer :global(.rail-theme-button[data-size='toolbar']) {
			flex-shrink: 0;
		}
		.rail-footer :global(.md-icon-btn[data-size='toolbar']) {
			width: var(--rail-control-size);
			min-width: var(--rail-control-size);
			height: var(--rail-control-size);
			min-height: var(--rail-control-size);
			border: 1px solid var(--md-sys-color-outline-variant);
			border-radius: var(--md-sys-shape-medium);
			background: var(--md-sys-color-surface-container);
		}
		.rail-footer :global(.md-icon-btn[data-size='toolbar']:hover) {
			background: var(--md-sys-color-surface-container-high);
		}

		.compact-workspace-nav {
			display: none;
		}
		.content {
			grid-column: 2;
			grid-row: 2;
			min-width: 0;
			padding: var(--md-sys-space-xl);
			background:
				radial-gradient(
					ellipse at 58% -18%,
					color-mix(in srgb, var(--md-sys-color-primary) 9%, transparent),
					transparent 56%
				),
				var(--md-sys-color-surface);
		}
		.content--chat {
			padding: 0;
		}
		:global(.content:not(.content--chat) .page-shell) {
			max-width: none;
			min-height: 100%;
		}
		:global(.content:not(.content--chat) .page-shell .tools-page),
		:global(.content:not(.content--chat) .page-shell .memory-page),
		:global(.content:not(.content--chat) .page-shell .settings-page) {
			max-width: none;
		}
		:global(.content:not(.content--chat) .workspace-surface) {
			min-height: 100%;
			padding: var(--md-sys-space-2xl);
		}
	}

	@media screen and (min-width: 600px) and (max-width: 900px) {
		.app-shell {
			--workspace-rail-width: 72px;
		}
		.rail-brand,
		.workspace-rail :global(.workspace-link) {
			justify-content: center;
			padding-inline: var(--md-sys-space-xs);
		}
		.rail-brand :global(.wordmark),
		.workspace-rail :global(.workspace-link__copy),
		.rail-footer :global(.status-text) {
			display: none;
		}
		.rail-footer {
			display: grid;
			grid-template-columns: minmax(0, 1fr);
			justify-items: center;
			gap: var(--md-sys-space-xs);
		}
		.rail-footer :global(.status-switch) {
			display: contents;
		}
		.rail-footer :global(.status-chip) {
			justify-content: center;
			width: var(--rail-control-size);
			padding-inline: var(--md-sys-space-xs);
		}
		.rail-footer :global(.task-action),
		.rail-footer :global(.rail-theme-button[data-size='toolbar']) {
			grid-column: 1;
			grid-row: auto;
			justify-self: center;
		}
	}

	@media screen and (min-width: 600px) and (orientation: landscape) {
		.app-shell {
			grid-template-rows: minmax(0, 1fr);
		}
		.titlebar {
			display: none;
		}
		.workspace-rail {
			grid-row: 1;
		}
		.content {
			grid-row: 1;
		}
	}

	@media screen and (orientation: portrait) {
		.titlebar {
			background: var(--md-sys-color-surface);
		}
	}
</style>
