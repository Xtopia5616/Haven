<script>
	// Cross-session messaging view: agents discovered by the shared inbox bus
	// (see crates/tools/src/inbox.rs) and their message history. Read-only —
	// the bus itself is only ever written by agent tools / the ReAct loop.
	// Card styling follows ToolResultCard (.tool-card) so the panel looks
	// like the tool-call cards in chat.
	import { onMount, onDestroy } from 'svelte';
	import { invoke } from '$lib/tauri.ts';
	import logger from '$lib/logger.ts';
	import StatusDot from '$lib/StatusDot.svelte';
	import JsonView from '$lib/JsonView.svelte';

	/** @typedef {{ name: string; last_seen: string; started_at: string; status: 'online'|'offline'; title?: string|null; capabilities: string[] }} MessagingAgent */
	/** @typedef {{ id: string; type: string; from: string; to: string; reply_address?: string|null; in_reply_to?: string|null; thread_id?: string|null; subject?: string|null; text: string; payload?: any|null; created_at: string; expires_at?: string|null }} MessagingMessage */

	/** @type {MessagingAgent[]} */
	let agents = $state([]);
	let loading = $state(false);
	let error = $state('');
	/** @type {MessagingMessage[]} */
	let messages = $state([]);
	let loadingMessages = $state(false);
	let selectedName = $state('');
	/** @type {ReturnType<typeof setInterval> | null} */
	let pollTimer = null;

	/** @type {Record<string, string>} */
	const TYPE_LABELS = {
		message: '消息',
		reply: '回复',
		broadcast: '广播',
		request: '请求',
		system: '系统',
		receipt: '已读回执',
	};

	async function loadAgents() {
		loading = true;
		try {
			const res = await invoke('list_messaging_agents');
			/** @type {MessagingAgent[]} */
			const list = res?.agents ?? [];
			agents = list;
			if (selectedName && !list.some((/** @type {MessagingAgent} */ a) => a.name === selectedName)) {
				selectedName = '';
				messages = [];
			}
		} catch (e) {
			logger.warn('messaging', 'list_messaging_agents error', e);
			error = String(e);
		} finally {
			loading = false;
		}
	}

	/** @param {string} name */
	async function loadHistory(name) {
		loadingMessages = true;
		error = '';
		try {
			const res = await invoke('get_messaging_history', { name, limit: 200 });
			/** @type {MessagingMessage[]} */
			messages = res?.messages ?? [];
		} catch (e) {
			logger.warn('messaging', 'get_messaging_history error', e);
			error = String(e);
		} finally {
			loadingMessages = false;
		}
	}

	/** @param {string} name */
	function selectAgent(name) {
		selectedName = name;
		loadHistory(name);
	}

	/** @param {string} iso */
	function fmtTime(iso) {
		if (!iso) return '—';
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		const now = Date.now();
		const diff = now - d.getTime();
		if (diff < 60_000) return '刚刚';
		if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
		if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
		return d.toLocaleString();
	}

	/** @param {string} iso */
	function fmtFullTime(iso) {
		if (!iso) return '—';
		const d = new Date(iso);
		return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
	}

	/** @param {string} name */
	function shortName(name) {
		return name.length > 16 ? `${name.slice(0, 8)}…${name.slice(-6)}` : name;
	}

	onMount(() => {
		loadAgents();
		pollTimer = setInterval(loadAgents, 30_000);
		return () => {
			if (pollTimer) clearInterval(pollTimer);
		};
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});
</script>

<div class="messaging-view">
	<div class="messaging-header">
		<h2>跨会话消息</h2>
		<button class="ghost-btn" onclick={() => { loadAgents(); if (selectedName) loadHistory(selectedName); }} disabled={loading}>
			{loading ? '刷新中…' : '刷新'}
		</button>
	</div>

	{#if error}
		<p class="tool-card-empty messaging-error">{error}</p>
	{/if}

	<div class="messaging-layout">
		<section class="agent-panel" aria-label="Agent 列表">
			{#if agents.length === 0}
				<p class="tool-card-empty">暂无 agent 注册。会话首次使用消息工具后会自动出现在这里。</p>
			{:else}
				<ul class="agent-list">
					{#each agents as agent (agent.name)}
						<li>
							<button
								class="agent-card"
								class:selected={agent.name === selectedName}
								onclick={() => selectAgent(agent.name)}
							>
								<span class="agent-row">
									<StatusDot color={agent.status === 'online' ? 'success' : 'error'} />
									<span class="agent-name" title={agent.name}>{shortName(agent.name)}</span>
									<span class="agent-status">{agent.status === 'online' ? '在线' : '离线'}</span>
								</span>
								{#if agent.title}
									<span class="agent-title">{agent.title}</span>
								{/if}
								<span class="agent-meta">心跳 {fmtTime(agent.last_seen)}</span>
							</button>
						</li>
					{/each}
				</ul>
			{/if}
		</section>

		<section class="history-panel" aria-label="消息记录">
			{#if !selectedName}
				<p class="tool-card-empty">选择左侧一个 agent 查看它的消息记录（含已读归档）。</p>
			{:else if loadingMessages}
				<p class="tool-card-empty">加载中…</p>
			{:else if messages.length === 0}
				<p class="tool-card-empty">该 agent 还没有消息。</p>
			{:else}
				<ul class="msg-list">
					{#each messages as msg (msg.id)}
						<li class="tool-card msg-card" class:receipt={msg.type === 'receipt'}>
							<div class="tool-card-header">
								<span class="msg-badge">{TYPE_LABELS[msg.type] ?? msg.type}</span>
								<span class="tool-card-label">
									{msg.from === selectedName ? '收到' : '发出'} · {shortName(msg.from)} → {shortName(msg.to)}
								</span>
								<span class="msg-time">{fmtFullTime(msg.created_at)}</span>
							</div>
							{#if msg.subject}
								<div class="msg-subject">{msg.subject}</div>
							{/if}
							{#if msg.text}
								<p class="msg-text">{msg.text}</p>
							{/if}
							<div class="msg-meta">
								{#if msg.in_reply_to}
									<span class="msg-ref">回复 {shortName(msg.in_reply_to)}</span>
								{/if}
								{#if msg.thread_id}
									<span class="msg-ref">线程 {shortName(msg.thread_id)}</span>
								{/if}
								{#if msg.reply_address && msg.reply_address !== msg.from}
									<span class="msg-ref">回复地址 {shortName(msg.reply_address)}</span>
								{/if}
							</div>
							{#if msg.payload != null}
								<details class="msg-payload">
									<summary>载荷</summary>
									<JsonView value={msg.payload} />
								</details>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	</div>
</div>

<style>
	.messaging-view {
		display: flex;
		flex-direction: column;
		gap: 12px;
		height: 100%;
		min-height: 0;
	}
	.messaging-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}
	.messaging-header h2 {
		margin: 0;
		font-size: 1.05rem;
	}
	.ghost-btn {
		background: transparent;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: 8px;
		padding: 4px 12px;
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
		font-size: 0.85rem;
	}
	.ghost-btn:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.messaging-layout {
		display: grid;
		grid-template-columns: 240px 1fr;
		gap: 12px;
		flex: 1;
		min-height: 0;
	}
	.agent-panel,
	.history-panel {
		overflow-y: auto;
		min-height: 0;
	}
	.agent-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}
	.agent-card {
		width: 100%;
		text-align: left;
		background: transparent;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: 8px;
		padding: 8px 10px;
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.agent-card:hover {
		background: var(--md-sys-color-surface-variant);
	}
	.agent-card.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary-container);
	}
	.agent-row {
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.agent-name {
		font-family: var(--md-sys-typescale-body-medium-font);
		font-weight: 600;
		font-size: 0.85rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
	}
	.agent-status {
		font-size: 0.75rem;
		opacity: 0.7;
	}
	.agent-title {
		font-size: 0.8rem;
		opacity: 0.8;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.agent-meta {
		font-size: 0.72rem;
		opacity: 0.6;
	}
	.msg-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}
	.msg-card {
		padding: 10px 12px;
	}
	.msg-card.receipt {
		opacity: 0.75;
	}
	.msg-badge {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		border-radius: 4px;
		padding: 1px 6px;
		font-size: 0.72rem;
		margin-right: 6px;
		flex-shrink: 0;
	}
	.msg-time {
		margin-left: auto;
		font-size: 0.75rem;
		opacity: 0.65;
		flex-shrink: 0;
	}
	.msg-subject {
		font-weight: 600;
		font-size: 0.9rem;
		margin-top: 6px;
	}
	.msg-text {
		margin: 6px 0 0;
		font-size: 0.88rem;
		white-space: pre-wrap;
		word-break: break-word;
	}
	.msg-meta {
		display: flex;
		gap: 10px;
		flex-wrap: wrap;
		margin-top: 6px;
	}
	.msg-ref {
		font-size: 0.72rem;
		opacity: 0.65;
		font-family: var(--md-sys-typescale-body-small-font);
	}
	.msg-payload {
		margin-top: 6px;
		font-size: 0.8rem;
	}
	.messaging-error {
		color: var(--md-sys-color-error);
	}
</style>
