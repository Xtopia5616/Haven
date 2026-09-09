<script>
	import Logo from './Logo.svelte';
	import RecordingIndicator from './RecordingIndicator.svelte';
	import NotificationToast from './NotificationToast.svelte';
	import WorkspaceNav from './WorkspaceNav.svelte';
	import MaterialButton from './MaterialButton.svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';
	import { dragScroll } from '$lib/dragScroll.ts';

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
		overlay = {},
		duration = 0,
		onCancelRecording = null,
		status,
		content,
	} = $props();
</script>

<div class="app-shell">
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
		<div class="titlebar-right">
			{@render status?.()}
			<MaterialIconButton
				size="toolbar"
				variant="ghost"
				onclick={() => onToggleTheme?.()}
				label="切换主题"
				title={theme === 'dark' ? '切换到亮色模式' : '切换到暗色模式'}
				icon={theme === 'dark' ? 'sun' : 'moon'}
			></MaterialIconButton>
		</div>
	</header>

	<WorkspaceNav {tabs} {activeTab} {onNavigate} />

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
</div>

<style>
	.app-shell {
		display: flex;
		flex-direction: column;
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
		flex: 1;
		min-width: 0;
		min-height: 0;
		overflow-y: auto;
		overflow-x: clip;
		overscroll-behavior-x: none;
		touch-action: pan-y;
		padding: var(--md-sys-content-gutter);
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
	@media (max-width: 640px) {
		.titlebar {
			padding: 0 var(--md-sys-space-md);
		}
	}
</style>
