<script lang="ts">
	import Icon from './Icon.svelte';
	import MaterialButton from './MaterialButton.svelte';
	import type { SessionSummary } from './sessionReducer/types.ts';

	interface Props {
		sessions?: SessionSummary[];
		activeSessionId?: string | null;
		statusLabel?: (session: SessionSummary) => string;
		onNew?: () => void;
		onSelect?: (sessionId: string) => void;
	}

	let {
		sessions = [],
		activeSessionId = null,
		statusLabel = (session) => session.status,
		onNew = () => {},
		onSelect = () => {},
	}: Props = $props();
	let query = $state('');

	// The reducer may retain the active completed session briefly so the
	// conversation can show its termination message. It is no longer switchable
	// from the live-session rail; persisted history remains available separately.
	const switchableSessions = $derived(
		sessions.filter((session) => session.status !== 'completed'),
	);

	const filteredSessions = $derived.by(() => {
		const normalized = query.trim().toLocaleLowerCase();
		if (!normalized) return switchableSessions;
		return switchableSessions.filter((session) => {
			const input = typeof session.input === 'string' ? session.input : '';
			return `${sessionTitle(session)} ${input} ${session.id}`
				.toLocaleLowerCase()
				.includes(normalized);
		});
	});

	function sessionTitle(session: SessionSummary): string {
		const title = session.title || session.input;
		return typeof title === 'string' && title.trim() ? title.trim() : '新会话';
	}

	function sessionTone(session: SessionSummary): string {
		if (session.status === 'running' || session.status === 'pending') return 'active';
		if (session.status === 'paused') return 'waiting';
		if (session.status === 'error') return 'error';
		return 'quiet';
	}
</script>

<aside class="session-rail" aria-label="会话列表">
	<div class="session-rail__heading">
		<div>
			<p class="session-rail__eyebrow">对话空间</p>
			<h2>会话</h2>
		</div>
		<span class="session-rail__count" aria-label={`${switchableSessions.length} 个会话`}
			>{switchableSessions.length}</span
		>
	</div>

	<MaterialButton variant="filled" className="session-rail__new" onclick={() => onNew?.()}>
		{#snippet children()}
			<Icon name="plus" size={17} />
			<span>新建会话</span>
		{/snippet}
	</MaterialButton>

	<label class="session-rail__search">
		<Icon name="search" size={17} />
		<input bind:value={query} type="search" placeholder="搜索会话" aria-label="搜索会话" />
		{#if query}
			<button
				type="button"
				class="session-rail__clear"
				aria-label="清除搜索"
				onclick={() => (query = '')}
			>
				<Icon name="close" size={14} />
			</button>
		{/if}
	</label>

	<div class="session-rail__list-heading">
		<span>可切换的会话</span>
		<span>{filteredSessions.length}</span>
	</div>
	<div class="session-rail__scroll">
		{#if filteredSessions.length > 0}
			<ul class="session-rail__list">
				{#each filteredSessions as session (session.id)}
					<li>
						<button
							type="button"
							class="session-rail__item"
							class:selected={activeSessionId === session.id}
							aria-current={activeSessionId === session.id ? 'page' : undefined}
							title={sessionTitle(session)}
							onclick={() => onSelect?.(session.id)}
						>
							<span
								class="session-rail__item-mark"
								data-tone={sessionTone(session)}
								aria-hidden="true"
							>
								<Icon name="chat" size={15} />
							</span>
							<span class="session-rail__item-copy">
								<span class="session-rail__item-title">{sessionTitle(session)}</span
								>
								<span class="session-rail__item-status">{statusLabel(session)}</span
								>
							</span>
						</button>
					</li>
				{/each}
			</ul>
		{:else}
			<div class="session-rail__empty">
				<Icon name={query ? 'search' : 'chat'} size={20} />
				<strong>{query ? '没有匹配的会话' : '从一次新对话开始'}</strong>
				<span>{query ? '试试更短的关键词' : '你的对话会显示在这里'}</span>
			</div>
		{/if}
	</div>
</aside>

<style>
	.session-rail {
		display: flex;
		flex-direction: column;
		width: 100%;
		min-width: 0;
		min-height: 0;
		padding: var(--md-sys-space-lg) var(--md-sys-space-md) var(--md-sys-space-md);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface);
	}
	.session-rail__heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-lg);
		padding-inline: var(--md-sys-space-xs);
	}
	.session-rail__eyebrow {
		margin: 0 0 var(--md-sys-space-xs);
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.session-rail h2 {
		margin: 0;
		font-size: var(--md-sys-typescale-headline-medium-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-headline-medium-line-height);
	}
	.session-rail__count {
		display: grid;
		place-items: center;
		min-width: 28px;
		height: 28px;
		padding-inline: var(--md-sys-space-xs);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-variant-numeric: tabular-nums;
	}
	:global(.md-btn.session-rail__new) {
		gap: var(--md-sys-space-sm);
		justify-content: center;
		width: 100%;
		margin-bottom: var(--md-sys-space-md);
	}
	.session-rail__search {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-height: 42px;
		padding-inline: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface);
		color: var(--md-sys-color-on-surface-variant);
		transition: border-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.session-rail__search:focus-within {
		border-color: var(--md-sys-color-primary);
	}
	.session-rail__search input {
		width: 100%;
		min-width: 0;
		border: 0;
		outline: 0;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font: inherit;
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.session-rail__search input::placeholder {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.8;
	}
	.session-rail__clear {
		display: grid;
		place-items: center;
		width: 26px;
		height: 26px;
		flex: 0 0 26px;
		padding: 0;
		border: 0;
		border-radius: var(--md-sys-shape-full);
		background: transparent;
		color: inherit;
		cursor: pointer;
	}
	.session-rail__clear:hover {
		background: var(--md-sys-color-surface-container-high);
	}
	.session-rail__list-heading {
		display: flex;
		justify-content: space-between;
		padding: var(--md-sys-space-lg) var(--md-sys-space-xs) var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.session-rail__scroll {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		overscroll-behavior: contain;
		scrollbar-color: var(--md-sys-color-outline-variant) transparent;
		scrollbar-width: thin;
	}
	.session-rail__list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.session-rail__item {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		min-height: 60px;
		padding: var(--md-sys-space-sm);
		border: 1px solid transparent;
		border-radius: var(--md-sys-shape-medium);
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font: inherit;
		text-align: left;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.session-rail__item:hover {
		background: var(--md-sys-color-surface-container);
	}
	.session-rail__item:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: 1px;
	}
	.session-rail__item.selected {
		border-color: color-mix(in srgb, var(--md-sys-color-primary) 24%, transparent);
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 72%,
			var(--md-sys-color-surface) 28%
		);
	}
	.session-rail__item-mark {
		display: grid;
		place-items: center;
		width: 34px;
		height: 34px;
		flex: 0 0 34px;
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.session-rail__item-mark[data-tone='active'] {
		background: color-mix(in srgb, var(--md-sys-color-primary) 12%, transparent);
		color: var(--md-sys-color-primary);
	}
	.session-rail__item-mark[data-tone='waiting'] {
		background: color-mix(in srgb, var(--md-sys-color-warning) 12%, transparent);
		color: var(--md-sys-color-warning);
	}
	.session-rail__item-mark[data-tone='error'] {
		background: color-mix(in srgb, var(--md-sys-color-error) 10%, transparent);
		color: var(--md-sys-color-error);
	}
	.session-rail__item-copy {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
		gap: 4px;
	}
	.session-rail__item-title {
		overflow: hidden;
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-small-line-height);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.session-rail__item-status {
		overflow: hidden;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.session-rail__empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-3xl) var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface-variant);
		text-align: center;
	}
	.session-rail__empty strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.session-rail__empty span {
		font-size: var(--md-sys-typescale-label-small-size);
	}
</style>
