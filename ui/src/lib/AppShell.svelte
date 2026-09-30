<script lang="ts">
	import type { Snippet } from 'svelte';
	import Logo from './Logo.svelte';
	import RecordingIndicator from './RecordingIndicator.svelte';
	import NotificationToast from './NotificationToast.svelte';
	import WorkspaceNav from './WorkspaceNav.svelte';
	import MaterialButton from './MaterialButton.svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';
	import Icon from './Icon.svelte';
	import GlobalContextMenu from './GlobalContextMenu.svelte';
	import LandscapeWorkspaceNav from './LandscapeWorkspaceNav.svelte';
	import { dragScroll } from '$lib/dragScroll.ts';
	import type { RecordingOverlayState } from '$lib/runtimeStateStore.ts';

	interface WorkspaceTab {
		id: string;
		label: string;
		hint?: string;
		icon?: string;
	}

	interface Props {
		activeTab?: string;
		tabs?: WorkspaceTab[];
		theme?: string;
		onToggleTheme?: () => void;
		onNavigate?: (tabId: string) => void;
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
		theme = 'dark',
		onToggleTheme = () => {},
		onNavigate = () => {},
		overlay = {
			visible: false,
			isRecording: false,
			processing: false,
			sessionId: null,
			startedAt: null,
			reason: null,
			vadState: 'silent',
		},
		duration = 0,
		onCancelRecording = null,
		status,
		content,
	}: Props = $props();

</script>

{#snippet themeControl()}
	<MaterialIconButton
		className="rail-theme-button"
		size="toolbar"
		variant="ghost"
		onclick={() => onToggleTheme?.()}
		label="切换主题"
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
				ariaLabel="回到对话"
				title="回到对话"
				onclick={() => onNavigate('chat')}
			>
				{#snippet children()}
					<Logo size={22} withText={true} />
				{/snippet}
			</MaterialButton>
		</div>
		<div class="titlebar-heading" aria-live="polite">
			<strong>{tabs.find((tab) => tab.id === activeTab)?.label || '工作区'}</strong>
		</div>
		<div class="titlebar-right">
			{@render shellControls()}
		</div>
	</header>

	<aside class="workspace-rail" aria-label="Haven 主导航">
		<button
			type="button"
			class="rail-brand"
			aria-label="返回聊天工作区"
			title="返回聊天工作区"
			onclick={() => onNavigate('chat')}
		>
			<Logo size={30} withText={true} />
		</button>
		<div class="rail-section-label">工作区</div>
		<LandscapeWorkspaceNav {tabs} {activeTab} {onNavigate} />
		<div class="rail-footer">
			{@render shellControls()}
		</div>
	</aside>

	<div class="compact-workspace-nav">
		<WorkspaceNav {tabs} {activeTab} {onNavigate} />
	</div>

	<main class="content" class:content--chat={activeTab === 'chat'} use:dragScroll={{ axis: 'y' }}>
		{@render content?.()}
	</main>

	<RecordingIndicator
		isRecording={overlay.isRecording}
		processing={overlay.processing}
		{duration}
		vadState={overlay.vadState}
		reason={overlay.reason}
		onCancel={onCancelRecording}
	/>
	<NotificationToast />
	<GlobalContextMenu />
</div>

<style>
	.app-shell {
		display: flex;
		flex-direction: column;
		grid-template-columns: 0px minmax(0, 1fr);
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
	.titlebar-heading {
		display: none;
		min-width: 0;
		flex: 1;
		flex-direction: column;
		align-items: flex-start;
		gap: 2px;
		padding-left: var(--md-sys-space-lg);
		-webkit-app-region: no-drag;
	}
	.titlebar-heading > strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-title-large-line-height);
	}
	.workspace-rail {
		display: none;
		transition: padding var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
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
	.content--chat {
		overflow: hidden;
		overflow-x: clip;
		padding: 0;
		display: flex;
		flex-direction: column;
	}
	:global(.content.drag-scroll--active) {
		cursor: grabbing;
		user-select: none;
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
	 * conversation layout. */
	:global(.content:not(.content--chat) .page-shell) {
		max-width: clamp(640px, 92vw, var(--md-sys-content-max-width));
		min-width: 0;
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
	@media (prefers-reduced-motion: reduce) {
		:global(.chat-tab-enter) {
			animation-duration: 1ms;
		}
		:global(.tab-panel--leaving) {
			animation-duration: 1ms;
		}
	}
	@media (max-width: 640px) {
		.titlebar {
			padding: 0 var(--md-sys-space-md);
		}
	}

	@media screen and (min-aspect-ratio: 1/1) and (orientation: landscape) {
		.app-shell {
			display: grid;
			grid-template-columns: 76px minmax(0, 1fr);
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
			display: none;
		}
		.titlebar-heading {
			display: flex;
		}
		.titlebar-right {
			display: none;
		}
		.workspace-rail {
			grid-column: 1;
			grid-row: 1 / span 2;
			display: flex;
			flex-direction: column;
			align-items: center;
			gap: var(--md-sys-space-lg);
			min-width: 0;
			padding: var(--md-sys-space-md) var(--md-sys-space-sm);
			border-right: 1px solid var(--md-sys-color-outline-variant);
			background: var(--md-sys-color-surface-container-low);
		}
		.rail-brand {
			display: grid;
			place-items: center;
			width: 52px;
			height: 52px;
			padding: 0;
			border: 0;
			border-radius: var(--md-sys-shape-medium);
			background: transparent;
			cursor: pointer;
			-webkit-app-region: no-drag;
		}
		.rail-brand:hover {
			background: var(--md-sys-color-surface-container-high);
		}
		.rail-brand :global(.wordmark),
		.rail-section-label {
			display: none;
		}
		.rail-section-label {
			color: var(--md-sys-color-on-surface-variant);
			font-size: var(--md-sys-typescale-label-small-size);
			font-weight: 700;
			letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		}
		.workspace-rail :global(.landscape-workspace-nav) {
			align-items: center;
			width: 100%;
			gap: var(--md-sys-space-sm);
		}
		.workspace-rail :global(.workspace-link) {
			justify-content: center;
			width: 52px;
			min-height: 52px;
			padding: 0;
			border-radius: var(--md-sys-shape-medium);
		}
		.workspace-rail :global(.workspace-link__copy) {
			/* A zero-width flex item still leaves the button gap behind, which
			 * nudges the icon off center in the compact landscape rail. */
			display: none;
		}
		.workspace-rail :global(.workspace-link.active) {
			border-color: transparent;
		}
		.rail-footer {
			display: grid;
			grid-template-columns: repeat(2, var(--md-comp-icon-button-dense-size));
			grid-template-rows: var(--md-comp-icon-button-dense-size) auto;
			align-items: center;
			justify-content: center;
			gap: var(--md-sys-space-2xs);
			width: 100%;
			margin-top: auto;
			padding-top: var(--md-sys-space-md);
			border-top: 1px solid var(--md-sys-color-outline-variant);
		}
		.rail-footer :global(.status-switch) {
			display: contents;
		}
		.rail-footer :global(.status-chip) {
			grid-column: 1;
			grid-row: 1;
			justify-content: center;
			width: var(--md-comp-icon-button-dense-size);
			min-width: var(--md-comp-icon-button-dense-size);
			height: var(--md-comp-icon-button-dense-size);
			padding: 0;
		}
		.rail-footer :global(.status-text) {
			display: none;
		}
		.rail-footer :global(.task-action) {
			grid-column: 1 / span 2;
			grid-row: 2;
			justify-content: center;
		}
		.rail-footer :global(.task-action .md-icon-btn[data-size='toolbar']) {
			width: var(--md-comp-icon-button-dense-size);
			min-width: var(--md-comp-icon-button-dense-size);
			height: var(--md-comp-icon-button-dense-size);
			min-height: var(--md-comp-icon-button-dense-size);
		}
		.rail-footer :global(.rail-theme-button[data-size='toolbar']) {
			grid-column: 2;
			grid-row: 1;
			justify-self: center;
			width: var(--md-comp-icon-button-dense-size);
			min-width: var(--md-comp-icon-button-dense-size);
			height: var(--md-comp-icon-button-dense-size);
			min-height: var(--md-comp-icon-button-dense-size);
			border: 1px solid var(--md-sys-color-outline-variant);
			border-radius: var(--md-comp-status-radius);
			background: var(--md-sys-color-surface-container-high);
			transition:
				background-color var(--md-sys-motion-duration-fast)
					var(--md-sys-motion-easing-standard),
				border-color var(--md-sys-motion-duration-fast)
					var(--md-sys-motion-easing-standard);
		}
		.rail-footer :global(.rail-theme-button[data-size='toolbar']:hover) {
			border-color: var(--md-sys-color-outline);
			background: var(--md-sys-color-surface-container-highest);
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

	@media screen and (min-aspect-ratio: 8/5) and (orientation: landscape) {
		.app-shell {
			grid-template-columns: 232px minmax(0, 1fr);
		}
		.workspace-rail {
			align-items: stretch;
			gap: var(--md-sys-space-md);
			padding: var(--md-sys-space-lg) var(--md-sys-space-md);
		}
		.rail-brand {
			justify-content: flex-start;
			width: 100%;
			padding-inline: var(--md-sys-space-sm);
		}
		.rail-brand :global(.wordmark),
		.rail-section-label {
			display: initial;
		}
		.rail-section-label {
			display: block;
			padding: var(--md-sys-space-sm) var(--md-sys-space-sm) 0;
		}
		.workspace-rail :global(.landscape-workspace-nav) {
			align-items: stretch;
		}
		.workspace-rail :global(.workspace-link) {
			justify-content: flex-start;
			width: 100%;
			min-height: 56px;
			padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		}
		.workspace-rail :global(.workspace-link__copy) {
			display: flex;
			flex: 1 1 auto;
			max-width: 14rem;
			opacity: 1;
			transform: none;
			pointer-events: auto;
		}
		.rail-footer {
			display: flex;
			flex-direction: row;
			align-items: center;
			width: 100%;
			padding-inline: var(--md-sys-space-sm);
		}
		.rail-footer :global(.status-switch) {
			display: inline-flex;
			flex-direction: row;
			align-items: center;
			gap: var(--md-sys-space-xs);
			width: calc(100% - var(--md-comp-icon-button-size) - var(--md-sys-space-xs));
			min-width: 0;
		}
		.rail-footer :global(.status-chip) {
			justify-content: flex-start;
			width: auto;
			min-width: 0;
			height: var(--md-comp-status-height);
			flex: 1 1 auto;
			max-width: 100%;
			padding-inline: var(--md-sys-space-sm);
		}
		.rail-footer :global(.status-text) {
			display: inline;
			min-width: 0;
			overflow: hidden;
			text-overflow: ellipsis;
			white-space: nowrap;
		}
		.rail-footer :global(.rail-theme-button) {
			align-self: center;
			margin-inline-start: auto;
			width: var(--md-comp-icon-button-size);
			min-width: var(--md-comp-icon-button-size);
			height: var(--md-comp-icon-button-size);
			min-height: var(--md-comp-icon-button-size);
		}
	}

	@media screen and (min-aspect-ratio: 21/9) {
		.app-shell {
			grid-template-columns: 232px minmax(0, calc(233.333vh - 232px));
			justify-content: center;
		}
	}
</style>
