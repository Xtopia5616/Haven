<script>
	import Logo from './Logo.svelte';
	import RecordingIndicator from './RecordingIndicator.svelte';
	import NotificationToast from './NotificationToast.svelte';
	import WorkspaceNav from './WorkspaceNav.svelte';
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
			<Logo size={22} withText={true} />
		</div>
		<div class="titlebar-right">
			{@render status?.()}
			<MaterialIconButton
				size="toolbar"
				variant="ghost"
				onclick={() => onToggleTheme?.()}
				label="切换主题"
				title={theme === 'dark' ? '切换到亮色模式' : '切换到暗色模式'}
			>
				{#if theme === 'dark'}
					<svg class="theme-icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
						<path
							d="M12 7a5 5 0 100 10 5 5 0 000-10zm0-5a1 1 0 011 1v2a1 1 0 11-2 0V3a1 1 0 011-1zm0 17a1 1 0 011 1v2a1 1 0 11-2 0v-2a1 1 0 011-1zM4.2 4.2a1 1 0 011.4 0l1.5 1.5A1 1 0 015.7 7.1L4.2 5.6a1 1 0 010-1.4zm12.7 12.7a1 1 0 011.4 0l1.5 1.5a1 1 0 11-1.4 1.4l-1.5-1.5a1 1 0 010-1.4zM2 12a1 1 0 011-1h2a1 1 0 110 2H3a1 1 0 01-1-1zm17 0a1 1 0 011-1h2a1 1 0 110 2h-2a1 1 0 01-1-1zM4.2 19.8a1 1 0 010-1.4l1.5-1.5a1 1 0 111.4 1.4l-1.5 1.5a1 1 0 01-1.4 0zm12.7-12.7a1 1 0 010-1.4l1.5-1.5a1 1 0 111.4 1.4l-1.5 1.5a1 1 0 01-1.4 0z"
						/>
					</svg>
				{:else}
					<svg class="theme-icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
						<path d="M21 12.8A9 9 0 1111.2 3a7 7 0 009.8 9.8z" />
					</svg>
				{/if}
			</MaterialIconButton>
		</div>
	</header>

	<WorkspaceNav {tabs} {activeTab} onNavigate={onNavigate} />

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
	.titlebar-right {
		gap: var(--md-sys-space-sm);
		-webkit-app-region: no-drag;
	}
	.theme-icon {
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
