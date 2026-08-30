<script>
	/**
	 * @typedef {{ id: string; title?: string; input_text?: string; transcript?: string; status: string; created_at: string; [key: string]: any }} MemorySession
	 */

	/** @type {MemorySession[]} */
	let sessions = $state([]);
	let searchQuery = $state('');
	/** @type {ReturnType<typeof setTimeout> | null} */
	let searchTimer = null;
	let deleteTarget = /** @type {Record<string, any> | null} */ ($state(null));
	let showClearDialog = $state(false);
	let selectMode = $state(false);
	let selectedIds = $state(new Set());
	let offset = $state(0);
	let totalCount = $state(0);
	let loading = $state(false);
	let hasMore = $state(true);
	let loadSessionsSeq = 0;
	let loadFactsSeq = 0;
	const PAGE_SIZE = 50;

	let statusFilter = $state('');
	let startDate = $state('');
	let endDate = $state('');
	let showDateFilter = $state(false);

	/** @type {string | null} */
	let editingTitle = $state(null);
	let renameValue = $state('');

	// Right-click context menu on a session item (open / rename / export / delete)
	/** @type {{ open: boolean; x: number; y: number; session: MemorySession | null }} */
	let ctxMenu = $state({ open: false, x: 0, y: 0, session: null });

	// Memory center sub-tabs: sessions + long-term facts + semantic recall.
	let activeTab = $state('sessions');
	const memoryTabs = [
		{ id: 'sessions', label: '会话' },
		{ id: 'facts', label: '事实' },
		{ id: 'recall', label: '检索' },
	];

	// Semantic / keyword recall over edges (fact) and items (episode).
	let memoryRecall = $state({
		query: '',
		kind: 'fact',
		results: /** @type {any[]} */ ([]),
		loading: false,
		searched: false,
	});

	// Facts = memory_edges SPO rows (thin Fact API). Preferences are facts
	// tagged `preference` (single exclusive memory channel).
	/** @type {any[]} */
	let facts = $state([]);
	let factsLoaded = $state(false);
	/** @type {'' | 'user' | 'inferred'} */
	let factSourceFilter = $state('');
	let newFact = $state({ predicate: '', object: '', tags: '' });
	let addingFact = $state(false);

	const factSourceOptions = [
		{ value: '', label: '全部来源' },
		{ value: 'user', label: '手动' },
		{ value: 'inferred', label: '推断' },
	];

	const todayISO = $derived.by(() => {
		const n = new Date();
		return `${n.getFullYear()}-${String(n.getMonth() + 1).padStart(2, '0')}-${String(n.getDate()).padStart(2, '0')}`;
	});

	import logger from '$lib/logger.ts';
	import { formatError } from '$lib/formatError.ts';
	import { buildResumeMessages, mergeLiveStreaming } from '$lib/resumeMessages.ts';
	import { formatMessageTime, addNotification, activeSessionIdStore, resumeTargetStore } from '$lib/stores.ts';
	import { clearAllSessionMessages, clearSessionMessages, updateSessionMessages } from '$lib/sessionMessages.ts';
	import { restoreSessionLlmUsage, restoreSessionTokenStats } from '$lib/sessionUsage.ts';
	import { statusVariant } from '$lib/sessionStatus.ts';
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { goto } from '$app/navigation';
	import { invoke } from '$lib/tauri.ts';
	import { registerSessionListener } from '$lib/events.ts';
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialDatePicker from '$lib/MaterialDatePicker.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';

	const statusOptions = [
		{ value: '', label: '全部状态' },
		{ value: 'completed', label: '已完成' },
		{ value: 'paused', label: '已暂停' },
		{ value: 'error', label: '错误' },
	];

	/** @type {{ dispose: () => void } | null} */
	let unlistenTitleUpdate = null;
	/** @type {Array<{ dispose: () => void }>} */
	let unlistenLifecycle = [];
	/** @type {ReturnType<typeof setTimeout> | null} */
	let reloadTimer = null;

	onMount(async () => {
		await loadSessions();
		unlistenTitleUpdate = await registerSessionListener('session:title-updated', (event) => {
			const { sessionId, title } = event.payload;
			sessions = sessions.map(t => t.id === sessionId ? { ...t, title } : t);
		}, { tag: 'memory' });
		// Memory view is keep-alive mounted: it never remounts when the user
		// switches back to this tab, so new conversations (and session
		// lifecycle changes) must refresh the list via events, not onMount.
		// Debounced: a chat turn can fire several session:updated in a row
		// (pending → running → paused) — one reload suffices.
		const scheduleReload = () => {
			if (reloadTimer) clearTimeout(reloadTimer);
			reloadTimer = setTimeout(loadSessions, 300);
		};
		unlistenLifecycle = await Promise.all([
			registerSessionListener('session:created', scheduleReload, { tag: 'memory' }),
			registerSessionListener('session:updated', scheduleReload, { tag: 'memory' }),
			registerSessionListener('session:completed', scheduleReload, { tag: 'memory' }),
			registerSessionListener('session:error', scheduleReload, { tag: 'memory' }),
		]);
	});

	onDestroy(() => {
		if (searchTimer) clearTimeout(searchTimer);
		if (reloadTimer) clearTimeout(reloadTimer);
		if (unlistenTitleUpdate) unlistenTitleUpdate.dispose();
		unlistenLifecycle.forEach((r) => r.dispose());
		unlistenLifecycle = [];
	});

	// Defer the facts table load until the 事实 tab is first opened, so a
	// sessions-only visit never pays the full list_facts scan. Reload when
	// the source filter changes after the first visit.
	$effect(() => {
		if (activeTab !== 'facts') return;
		factSourceFilter;
		loadFacts();
	});

	/**
	 * @param {Record<string, any>} extra
	 */
	function filterParams(extra) {
		return {
			query: searchQuery || null,
			status: statusFilter || null,
			startDate: startDate || null,
			endDate: endDate || null,
			...extra,
		};
	}

	async function loadSessions() {
		const seq = ++loadSessionsSeq;
		loading = true;
		try {
			const results = await invoke('search_history_filtered', filterParams({ limit: PAGE_SIZE, offset: 0 }));
			// Stale response guard: a newer loadSessions call superseded this one.
			if (seq !== loadSessionsSeq) return;
			sessions = results || [];
			totalCount = sessions.length;
			offset = PAGE_SIZE;
			hasMore = sessions.length >= PAGE_SIZE;
		} catch {
			if (seq !== loadSessionsSeq) return;
			sessions = [];
			totalCount = 0;
			hasMore = false;
			addNotification('加载会话列表失败', 'error', 3000);
		}
		if (seq === loadSessionsSeq) loading = false;
	}

	async function loadMore() {
		if (loading || !hasMore) return;
		const seq = loadSessionsSeq;
		loading = true;
		try {
			const more = await invoke('search_history_filtered', filterParams({ limit: PAGE_SIZE, offset }));
			// Stale guard: a filter/search change superseded this page fetch.
			if (seq !== loadSessionsSeq) return;
			if (more && more.length > 0) {
				sessions = [...sessions, ...more];
				offset += more.length;
				hasMore = more.length >= PAGE_SIZE;
				totalCount = sessions.length;
			} else {
				hasMore = false;
			}
		} catch {
			if (seq !== loadSessionsSeq) return;
			hasMore = false;
			addNotification('加载更多会话失败', 'error', 3000);
		}
		if (seq === loadSessionsSeq) loading = false;
	}

	async function handleSearchInput() {
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = setTimeout(loadSessions, 300);
	}

	function handleFilterChange() {
		loadSessions();
	}

	/**
	 * @param {string} v
	 */
	function handleStatusFilterChange(v) {
		statusFilter = v;
		handleFilterChange();
	}

	/**
	 * @param {string} v
	 */
	function handleRecallKindChange(v) {
		memoryRecall.kind = v;
	}

	/**
	 * @param {string} v
	 */
	function handleStartDateChange(v) {
		startDate = v;
		if (endDate && endDate < startDate) endDate = '';
		handleFilterChange();
	}

	/**
	 * @param {string} v
	 */
	function handleEndDateChange(v) {
		endDate = v;
		handleFilterChange();
	}

	/**
	 * @param {MemorySession} session
	 */
	async function resumeSession(session) {
		try {
			await invoke('reopen_session', { sessionId: session.id });
			const result = await invoke('get_session_for_resume', { sessionId: session.id });
			const dbMessages = buildResumeMessages(result);
			// Atomically merge DB messages with any in-memory streaming messages
			// that arrived concurrently (e.g. from background session streaming).
			updateSessionMessages(session.id, (existing) =>
				mergeLiveStreaming(dbMessages, existing)
			);
			restoreSessionTokenStats(session.id, result.usage, result.usage_estimated);
			restoreSessionLlmUsage(session.id, result.llm_usage);
			resumeTargetStore.set({ sessionId: session.id, summary: session.input_text, title: session.title, wasError: session.status === 'error' || session.status === 'failed' });
			await goto('/');
		} catch (e) {
			addNotification(`加载会话详情失败: ${formatError(e)}`, 'error', 4000);
		}
	}


	/**
	 * @param {string} sessionId
	 */
	async function deleteSession(sessionId) {
		try {
			await invoke('delete_session', { sessionId });
			sessions = sessions.filter((t) => t.id !== sessionId);
			totalCount = sessions.length;
			// If the deleted session was the active one, reset conversation state.
			const current = get(activeSessionIdStore);
			if (current === sessionId) {
				activeSessionIdStore.set(null);
				clearSessionMessages(sessionId);
			}
			addNotification('会话已删除', 'success', 2000);
		} catch (e) {
			addNotification(`删除失败: ${formatError(e)}`, 'error', 4000);
		}
		deleteTarget = null;
	}

	async function clearSessions() {
		try {
			const count = await invoke('clear_history');
			sessions = [];
			totalCount = 0;
			hasMore = false;
			activeSessionIdStore.set(null);
			// Wipe only per-session message lists: the un-sent draft (typed or
			// transcribed text that was never submitted) belongs to no session
			// and must survive a session wipe.
			clearAllSessionMessages();
			addNotification(`已清空 ${count} 条会话`, 'success', 3000);
		} catch {
			addNotification('清空会话失败', 'error', 4000);
		}
		showClearDialog = false;
	}

	function enterSelectMode() {
		selectMode = true;
		selectedIds = new Set();
	}

	function cancelSelectMode() {
		selectMode = false;
		selectedIds = new Set();
	}

	/**
	 * @param {string} sessionId
	 */
	function toggleSelect(sessionId) {
		const next = new Set(selectedIds);
		if (next.has(sessionId)) {
			next.delete(sessionId);
		} else {
			next.add(sessionId);
		}
		selectedIds = next;
	}

	function toggleSelectAll() {
		if (selectedIds.size === sessions.length) {
			selectedIds = new Set();
		} else {
			selectedIds = new Set(sessions.map((t) => t.id));
		}
	}


	/**
	 * @param {MemorySession} session
	 */
	function displayTitle(session) {
		if (session.title) return session.title;
		const text = session.input_text || '';
		const m = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
		return (m ? m[0].trim() : text.trim()) || '未命名会话';
	}

	/**
	 * @param {MemorySession} session
	 */
	function startEdit(session) {
		editingTitle = session.id;
		if (session.title) {
			renameValue = session.title;
		} else {
			const text = session.input_text || '';
			const m = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
			renameValue = m ? m[0].trim() : text.trim();
		}
	}

	function cancelEdit() {
		editingTitle = null;
		renameValue = '';
	}

	/**
	 * @param {string} sessionId
	 */
	async function saveTitle(sessionId) {
		const value = renameValue.trim();
		if (!value) { cancelEdit(); return; }
		try {
			await invoke('update_session_title', { sessionId, title: value });
			const t = sessions.find(t => t.id === sessionId);
			if (t) t.title = value;
		} catch (e) {
			addNotification(`重命名失败: ${formatError(e)}`, 'error', 3000);
		}
		cancelEdit();
	}

	/**
	 * @param {KeyboardEvent} e
	 * @param {string} sessionId
	 */
	function handleRenameKeydown(e, sessionId) {
		if (e.key === 'Enter') { e.preventDefault(); saveTitle(sessionId); }
		else if (e.key === 'Escape') { cancelEdit(); }
	}

	/**
	 * @param {MemorySession[]} sessionsToExport
	 */
	function downloadSessions(sessionsToExport) {
		const json = JSON.stringify(
			{
				exported_at: new Date().toISOString(),
				count: sessionsToExport.length,
				sessions: sessionsToExport,
			},
			null,
			2,
		);
		const blob = new Blob([json], { type: 'application/json' });
		const url = URL.createObjectURL(blob);
		const a = document.createElement('a');
		a.href = url;
		a.download = `haven-sessions-${new Date().toISOString().slice(0, 10)}.json`;
		a.click();
		URL.revokeObjectURL(url);
	}

	/**
	 * @param {MouseEvent} e
	 * @param {MemorySession} session
	 */
	function openCtxMenu(e, session) {
		e.preventDefault();
		e.stopPropagation();
		ctxMenu = { open: true, x: e.clientX, y: e.clientY, session };
	}

	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0, session: null };
	}

	let ctxMenuItems = $derived.by(() => {
		const session = ctxMenu.session;
		if (!session) return [];
		return [
			{ id: 'open', label: '打开', icon: 'open', action: () => resumeSession(session) },
			{ id: 'rename', label: '重命名', icon: 'edit', action: () => startEdit(session) },
			{ id: 'export', label: '导出', icon: 'export', action: () => downloadSessions([session]) },
			{ id: 'delete', label: '删除', icon: 'delete', danger: true, action: () => (deleteTarget = session) },
		];
	});

	function exportSelected() {
		const selectedSessions = sessions.filter((t) => selectedIds.has(t.id));
		downloadSessions(selectedSessions);
		cancelSelectMode();
	}

	async function loadFacts() {
		const seq = ++loadFactsSeq;
		const source = factSourceFilter || null;
		try {
			const rows = (await invoke('list_facts', { source })) || [];
			if (seq !== loadFactsSeq) return;
			facts = rows;
			factsLoaded = true;
		} catch {
			if (seq !== loadFactsSeq) return;
			facts = [];
			factsLoaded = true;
			logger.warn('memory', 'load facts error');
		}
	}

	/**
	 * @param {string} v
	 */
	function handleFactSourceFilterChange(v) {
		factSourceFilter = /** @type {'' | 'user' | 'inferred'} */ (v);
	}

	async function addFact() {
		const predicate = newFact.predicate.trim();
		const object = newFact.object.trim();
		if (!predicate || !object) {
			addNotification('请输入 predicate 和 object', 'error', 3000);
			return;
		}
		addingFact = true;
		try {
			const tags = newFact.tags
				.split(',')
				.map((t) => t.trim())
				.filter(Boolean);
			await invoke('add_fact', {
				subject: 'user',
				predicate,
				object,
				tags: tags.length ? tags : null,
			});
			newFact = { predicate: '', object: '', tags: '' };
			await loadFacts();
			addNotification('事实已保存', 'success', 2500);
		} catch (e) {
			addNotification(`添加事实失败: ${formatError(e)}`, 'error', 3000);
		} finally {
			addingFact = false;
		}
	}

	/**
	 * @param {string} factId
	 */
	async function deleteFact(factId) {
		try {
			await invoke('delete_fact', { factId });
			facts = facts.filter((f) => f.id !== factId);
		} catch (e) {
			addNotification(`删除事实失败: ${formatError(e)}`, 'error', 3000);
		}
	}

	async function runRecall() {
		const q = memoryRecall.query.trim();
		if (!q) return;
		memoryRecall.loading = true;
		try {
			memoryRecall.results = (await invoke('recall_memory', {
				query: q,
				kind: memoryRecall.kind,
				limit: 10,
			})) || [];
			memoryRecall.searched = true;
		} catch (e) {
			memoryRecall.results = [];
			memoryRecall.searched = true;
			addNotification(`记忆检索失败: ${formatError(e)}`, 'error', 4000);
		} finally {
			memoryRecall.loading = false;
		}
	}
</script>

<div class="memory-page">
	<div class="header-row">
		<h1>记忆</h1>
		{#if activeTab === 'sessions'}
			<span class="count-badge">已显示 {totalCount} 条</span>
			<div class="header-actions">
				{#if selectMode}
					<button
						class="md-btn md-btn--filled"
						onclick={exportSelected}
						disabled={selectedIds.size === 0}
					>
						导出选中（{selectedIds.size}）
					</button>
					<button class="md-btn md-btn--text" onclick={cancelSelectMode}>取消</button>
				{:else}
					<button class="md-btn md-btn--outlined" onclick={enterSelectMode}>导出</button>
					{#if sessions.length > 0}
						<button class="md-btn md-btn--danger" onclick={() => (showClearDialog = true)}>
							清空会话
						</button>
					{/if}
				{/if}
			</div>
		{/if}
	</div>

	<div class="md-tabs memory-tabs" role="tablist">
		{#each memoryTabs as tab}
			<button
				class="md-tab"
				class:active={activeTab === tab.id}
				role="tab"
				aria-selected={activeTab === tab.id}
				onclick={() => (activeTab = tab.id)}
			>
				{tab.label}
			</button>
		{/each}
	</div>

	{#if activeTab === 'sessions'}
	<div class="filter-bar">
		<input
			class="md-input"
			type="text"
			placeholder="搜索会话"
			bind:value={searchQuery}
			oninput={handleSearchInput}
			autocomplete="off"
		/>
		<div class="filter-controls">
			<MaterialSelect
				value={statusFilter}
				options={statusOptions}
				onChange={handleStatusFilterChange}
			/>
			<button class="md-btn md-btn--outlined" onclick={() => (showDateFilter = true)}>
				{#if startDate || endDate}
					日期：{startDate ? startDate.replace(/-/g, '/') : '…'} ~ {endDate ? endDate.replace(/-/g, '/') : '…'}
				{:else}
					日期筛选
				{/if}
			</button>
		</div>
	</div>

	{#if selectMode && sessions.length > 0}
		<div class="select-bar">
			<button class="select-all-row" onclick={toggleSelectAll}>
				<div class="md-checkbox-static" class:checked={selectedIds.size === sessions.length}></div>
				<span>全选（{sessions.length}）</span>
			</button>
		</div>
	{/if}

	{#if sessions.length === 0}
		<div class="empty-state">{loading ? '加载中…' : '暂无会话'}</div>
	{:else}
		<div class="session-list">
			{#each sessions as session (session.id)}
			{#if selectMode}
				<button
					class="session-item session-item-btn"
					class:selected={selectedIds.has(session.id)}
					onclick={() => toggleSelect(session.id)}
				>
	<div class="session-item-main">
		<div class="session-top-row">
			<div class="select-checkbox">
				<div class="md-checkbox-static" class:checked={selectedIds.has(session.id)}></div>
			</div>
			<div class="session-title-row">
				<span class="session-title">{displayTitle(session)}</span>
				<MaterialBadge variant={statusVariant(session.status)} text={session.status} />
			</div>
		</div>
		{#if session.transcript}
			<div class="session-message">"{session.transcript}"</div>
		{/if}
		<div class="session-meta">
			<span class="meta-date">{formatMessageTime(session.created_at)}</span>
		</div>
	</div>
				</button>
	{:else}
			<div
				class="session-item"
				class:selected={selectedIds.has(session.id)}
				role="button"
				tabindex="0"
				onclick={() => resumeSession(session)}
				onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), resumeSession(session))}
				oncontextmenu={(e) => openCtxMenu(e, session)}
			>
				<div class="session-item-main">
					<div class="session-title-row">
						{#if editingTitle === session.id}
							<!-- svelte-ignore a11y_autofocus -->
							<input
								type="text"
								class="md-input title-input"
								bind:value={renameValue}
								onkeydown={(e) => handleRenameKeydown(e, session.id)}
								onblur={() => saveTitle(session.id)}
								onclick={(e) => e.stopPropagation()}
								autofocus
								autocomplete="off"
							/>
						{:else}
							<span
								class="session-title"
								onclick={(e) => (e.stopPropagation(), startEdit(session))}
								onkeydown={(e) => e.key === 'Enter' && (e.stopPropagation(), startEdit(session))}
								role="button"
								tabindex="0"
							>
								{displayTitle(session)}
								<svg class="title-edit-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
									<path d="M17 3a2.85 2.85 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />
								</svg>
							</span>
						{/if}
						<MaterialBadge variant={statusVariant(session.status)} text={session.status} />
					</div>
					{#if session.transcript}
						<div class="session-message">"{session.transcript}"</div>
					{/if}
					<div class="session-meta">
						<span class="meta-date">{formatMessageTime(session.created_at)}</span>
						<button
							class="md-btn md-btn--xs md-btn--text delete-btn-meta"
							onclick={(e) => (e.stopPropagation(), deleteTarget = session)}
						>
							删除
						</button>
					</div>
				</div>
			</div>
		{/if}
			{/each}
		</div>

		{#if hasMore}
			<div class="load-more-row">
				<button class="md-btn md-btn--outlined" onclick={loadMore} disabled={loading}>
					{loading ? '加载中…' : '加载更多'}
				</button>
			</div>
		{/if}
	{/if}
	{:else if activeTab === 'facts'}
		<div class="section">
			<div class="toolbar">
				<h2>长期事实</h2>
				<div class="toolbar-actions">
					<MaterialSelect
						value={factSourceFilter}
						options={factSourceOptions}
						onChange={handleFactSourceFilterChange}
					/>
				</div>
			</div>
			<p class="model-hint">跨会话长期记忆（身份、偏好、工作区等）。可手动添加/删除；Agent 也可通过 memory 工具的 remember / forget 更新。</p>
			<input
				type="text"
				class="md-input"
				placeholder="谓词（如 email）"
				bind:value={newFact.predicate}
				autocomplete="off"
			/>
			<input
				type="text"
				class="md-input"
				placeholder="对象（如 alice@example.com）"
				bind:value={newFact.object}
				autocomplete="off"
			/>
			<input
				type="text"
				class="md-input"
				placeholder="标签（可选，逗号分隔）"
				bind:value={newFact.tags}
				autocomplete="off"
			/>
			<div class="add-fact-actions">
				<button class="md-btn md-btn--filled" onclick={addFact} disabled={addingFact}>
					{addingFact ? '添加中…' : '添加事实'}
				</button>
			</div>
			{#if factsLoaded && facts.length > 0}
				<div class="fact-list">
					{#each facts as fact}
						<div class="fact-row">
							<span class="fact-key">
								{#if fact.subject !== 'user'}{fact.subject}:{/if}{fact.predicate}
							</span>
							<span class="fact-value">
								{#if fact.source === 'inferred'}
									<span class="fact-tag fact-tag--inf">推断</span>
								{:else}
									<span class="fact-tag fact-tag--user">手动</span>
								{/if}
								{fact.object}
							</span>
							<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => deleteFact(fact.id)} title="删除事实">
								&times;
							</button>
						</div>
					{/each}
				</div>
			{:else if factsLoaded}
				<p class="model-hint">暂无事实。使用 Haven 后会自动抽取并显示在这里。</p>
			{/if}
		</div>
	{:else}
		<div class="section">
			<h2>记忆检索</h2>
			<p class="model-hint">检索已存储的事实边与情景条目。配置了 Embedding Model 时使用语义检索，否则回退到关键词匹配。</p>
			<input
				id="memory-recall-query"
				type="text"
				class="md-input"
				bind:value={memoryRecall.query}
				placeholder="检索内容，如：深色主题"
				onkeydown={(e) => { if (e.key === 'Enter') runRecall(); }}
				autocomplete="off"
			/>
			<div class="recall-actions">
				<MaterialSelect
					id="memory-recall-kind"
					value={memoryRecall.kind}
					options={[
						{ value: 'fact', label: '事实' },
						{ value: 'episode', label: '情景' },
					]}
					onChange={handleRecallKindChange}
				/>
				<button class="md-btn md-btn--filled" onclick={runRecall} disabled={memoryRecall.loading}>
					{memoryRecall.loading ? '检索中…' : '检索'}
				</button>
			</div>
			{#if memoryRecall.results.length > 0}
				<ul class="recall-results">
					{#each memoryRecall.results as r (r.entity_id + r.text)}
						<li>
							<span class="recall-score">{(r.score ?? 0).toFixed(2)}</span>
							<span class="recall-text">{r.text}</span>
						</li>
					{/each}
				</ul>
			{:else if memoryRecall.searched && !memoryRecall.loading}
				<p class="model-hint">无匹配结果。</p>
			{/if}
		</div>
	{/if}
</div>

<MaterialDialog
	open={showDateFilter}
	onClose={() => (showDateFilter = false)}
	title="日期筛选"
>
	{#snippet children()}
		<div class="date-filter-dialog">
			<div class="date-range-header">
				<span class="date-range-label">已选范围</span>
				<span class="date-range-value">
					{#if startDate || endDate}
						{startDate ? startDate.replace(/-/g, '/') : '…'} — {endDate ? endDate.replace(/-/g, '/') : '…'}
					{:else}
						全部日期
					{/if}
				</span>
			</div>
			<div class="date-input-row">
				<div class="date-field">
					<label class="date-filter-label" for="start-date">开始日期</label>
					<MaterialDatePicker
						id="start-date"
						value={startDate}
						max={todayISO}
						onChange={handleStartDateChange}
					/>
				</div>
				<div class="date-field">
					<label class="date-filter-label" for="end-date">结束日期</label>
					<MaterialDatePicker
						id="end-date"
						value={endDate}
						min={startDate || undefined}
						max={todayISO}
						onChange={handleEndDateChange}
					/>
				</div>
			</div>
		</div>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => { startDate = ''; endDate = ''; handleFilterChange(); }}>
			清除
		</button>
		<button class="md-btn md-btn--filled" onclick={() => (showDateFilter = false)}>
			完成
		</button>
	{/snippet}
</MaterialDialog>

<MaterialDialog
	open={deleteTarget !== null}
	onClose={() => (deleteTarget = null)}
	title="删除会话"
>
	{#snippet children()}
		<p class="dialog-text">
			确定删除「{deleteTarget?.title || deleteTarget?.input_text || '未命名会话'}」？此操作不可撤销。
		</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => (deleteTarget = null)}>取消</button>
		<button class="md-btn md-btn--danger" onclick={() => { if (deleteTarget) deleteSession(deleteTarget.id); }}>
			删除
		</button>
	{/snippet}
</MaterialDialog>

<MaterialDialog
	open={showClearDialog}
	onClose={() => (showClearDialog = false)}
	title="清空会话"
>
	{#snippet children()}
		<p class="dialog-text">
			将永久删除全部会话记录（长期事实不受影响）。此操作不可撤销。
		</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => (showClearDialog = false)}>取消</button>
		<button class="md-btn md-btn--danger" onclick={clearSessions}>清空全部</button>
	{/snippet}
</MaterialDialog>

<ContextMenu
	open={ctxMenu.open}
	x={ctxMenu.x}
	y={ctxMenu.y}
	items={ctxMenuItems}
	onClose={closeCtxMenu}
/>

<style>
	.memory-page {
		max-width: var(--md-sys-content-max-width);
	}
	.header-row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: var(--md-sys-space-xl);
	}
	h1 {
		font-size: 24px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
		margin: 0;
	}
	.header-actions {
		display: flex;
		gap: var(--md-sys-space-sm);
	}
	.filter-bar {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-xl);
	}
	.filter-bar > :global(.md-input) {
		flex: 1;
	}
	.filter-controls {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.filter-controls :global(.md-select-container) {
		width: 140px;
		flex-shrink: 0;
	}
	.filter-controls .md-btn--outlined {
		width: 120px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		display: inline-block;
		flex-shrink: 0;
	}
	.date-filter-dialog {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
	}
	.date-range-header {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		background: var(--md-sys-color-surface-container);
		border-radius: var(--md-sys-shape-medium);
	}
	.date-range-label {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: 0.5px;
		font-weight: 500;
	}
	.date-range-value {
		font-size: 20px;
		font-weight: 500;
		color: var(--md-sys-color-on-surface);
		line-height: 1.3;
	}
	.date-input-row {
		display: flex;
		gap: var(--md-sys-space-md);
	}
	.date-field {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.date-filter-label {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 500;
		padding-left: var(--md-sys-space-xs);
	}
	.select-bar {
		margin-bottom: var(--md-sys-space-md);
	}
	.select-all-row {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		background: none;
		border: none;
		font-family: inherit;
		cursor: pointer;
		padding: 0;
	}
	.empty-state {
		text-align: center;
		padding: 80px 0;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.session-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.session-item {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		transition: box-shadow var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		outline: none;
	}
	.session-item:hover {
		background: var(--md-sys-color-surface-container);
		border-color: var(--md-sys-color-outline);
	}
	.session-item:focus-visible {
		border-color: var(--md-sys-color-primary);
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--md-sys-color-primary) 30%, transparent);
	}
	.session-item.selected {
		background: var(--md-sys-color-primary-container);
		border-color: var(--md-sys-color-primary);
	}
	.session-item-main {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		cursor: pointer;
	}
	.session-top-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
	}
	.session-title-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 1;
		min-width: 0;
	}
	.session-title {
		font-size: 14px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
	}
	.session-title:hover .title-edit-icon {
		opacity: 1;
	}
	.title-edit-icon {
		opacity: 0;
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
		transition: opacity 0.15s;
	}
	.title-input {
		font-size: 14px;
		font-weight: 600;
		padding: 2px 6px;
		width: 280px;
	}
	.session-message {
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		background: var(--md-sys-color-surface-container);
		border-radius: var(--md-sys-shape-small);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.session-meta {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.75;
	}
	.meta-date {
		font-family: var(--md-sys-typescale-mono);
		font-size: 10px;
	}
	.delete-btn-meta {
		margin-left: auto;
	}
	.session-item-btn {
		width: 100%;
		text-align: left;
		font-family: inherit;
		cursor: pointer;
	}
	.session-item-btn:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: -2px;
	}
	.select-checkbox {
		flex-shrink: 0;
		display: flex;
		align-items: center;
	}
	.md-checkbox-static {
		width: 18px;
		height: 18px;
		border: 2px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-extra-small);
		background: transparent;
		transition: background-color var(--md-sys-motion-duration-fast)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
		position: relative;
		flex-shrink: 0;
	}
	.md-checkbox-static.checked {
		background: var(--md-sys-color-primary);
		border-color: var(--md-sys-color-primary);
	}
	.md-checkbox-static.checked::after {
		content: '';
		position: absolute;
		left: 50%;
		top: 50%;
		width: 5px;
		height: 9px;
		border: solid var(--md-sys-color-on-primary);
		border-width: 0 2px 2px 0;
		transform: translate(-50%, -60%) rotate(45deg);
	}
	.dialog-text {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		line-height: 1.5;
	}
	.count-badge {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.load-more-row {
		display: flex;
		justify-content: center;
		padding: var(--md-sys-space-lg) 0;
	}
	.memory-tabs {
		margin-bottom: var(--md-sys-space-xl);
	}
	.section {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		min-height: var(--md-comp-button-small-height);
	}
	.toolbar h2 {
		margin: 0;
		line-height: var(--md-comp-button-small-height);
	}
	.toolbar-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.toolbar-actions :global(.md-select-container) {
		width: 140px;
	}
	.section h2 {
		font-size: 18px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
		margin: 0;
	}
	.model-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: calc(-1 * var(--md-sys-space-sm));
		margin-bottom: var(--md-sys-space-md);
	}
	.recall-actions,
	.add-fact-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.recall-actions :global(.md-select-container) {
		width: 200px;
		flex-shrink: 0;
	}
	.recall-results {
		list-style: none;
		margin: var(--md-sys-space-sm) 0 var(--md-sys-space-md);
		padding: 0;
		max-height: 220px;
		overflow-y: auto;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-radius-md);
	}
	.recall-results li {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		font-size: 13px;
	}
	.recall-results li:last-child {
		border-bottom: none;
	}
	.recall-score {
		font-variant-numeric: tabular-nums;
		color: var(--md-sys-color-primary);
		min-width: 42px;
	}
	.recall-text {
		color: var(--md-sys-color-on-surface);
		overflow-wrap: anywhere;
	}
	.fact-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.fact-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) 0;
	}
	.fact-key {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
		font-weight: 500;
		min-width: 140px;
		flex-shrink: 0;
	}
	.fact-value {
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		flex: 1;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.fact-tag {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 20px;
		padding: 0 6px;
		border-radius: var(--md-sys-shape-small);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		flex-shrink: 0;
	}
	.fact-tag--user {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.fact-tag--inf {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
</style>
