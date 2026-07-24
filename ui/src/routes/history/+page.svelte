<script>
	let tasks = $state([]);
	let searchQuery = $state('');
	let searchTimer;
	let deleteTarget = $state(null);
	let showClearDialog = $state(false);
	let selectMode = $state(false);
	let selectedIds = $state(new Set());
	let offset = $state(0);
	let totalCount = $state(0);
	let loading = $state(false);
	let hasMore = $state(true);
	const PAGE_SIZE = 50;

	let statusFilter = $state('');
	let startDate = $state('');
	let endDate = $state('');
	let showDateFilter = $state(false);

	const todayISO = $derived(new Date().toISOString().slice(0, 10));

	import { onMount } from 'svelte';
	import { get } from 'svelte/store';
	import { goto } from '$app/navigation';
	import { invoke } from '$lib/tauri.js';
	import { taskMessagesStore, setTaskMessages, getTaskMessages, clearTaskMessages, reviewTargetStore, activeTaskIdStore, addNotification } from '$lib/stores.js';
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialDatePicker from '$lib/MaterialDatePicker.svelte';

	const statusOptions = [
		{ value: '', label: 'All' },
		{ value: 'completed', label: 'Completed' },
		{ value: 'paused', label: 'Paused' },
		{ value: 'cancelled', label: 'Cancelled' },
		{ value: 'error', label: 'Error' },
	];

	onMount(async () => {
		await loadHistory();
	});

	function filterParams(extra) {
		return {
			query: searchQuery || null,
			status: statusFilter || null,
			classification: null,
			startDate: startDate || null,
			endDate: endDate || null,
			...extra,
		};
	}

	async function loadHistory() {
		loading = true;
		try {
			const results = await invoke('search_history_filtered', filterParams({ limit: PAGE_SIZE, offset: 0 }));
			tasks = results || [];
			totalCount = tasks.length;
			offset = PAGE_SIZE;
			hasMore = tasks.length >= PAGE_SIZE;
		} catch {
			tasks = [];
			totalCount = 0;
			hasMore = false;
		}
		loading = false;
	}

	async function loadMore() {
		if (loading || !hasMore) return;
		loading = true;
		try {
			const more = await invoke('search_history_filtered', filterParams({ limit: PAGE_SIZE, offset }));
			if (more && more.length > 0) {
				tasks = [...tasks, ...more];
				offset += more.length;
				hasMore = more.length >= PAGE_SIZE;
			} else {
				hasMore = false;
			}
		} catch {
			hasMore = false;
		}
		loading = false;
	}

	async function handleSearchInput() {
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = setTimeout(loadHistory, 300);
	}

	function handleFilterChange() {
		loadHistory();
	}

	function statusVariant(status) {
		const map = {
			completed: 'success',
			failed: 'error',
			error: 'error',
			cancelled: 'default',
			running: 'primary',
			paused: 'warning',
		};
		return map[status] || 'default';
	}

	async function reviewTask(task) {
		try {
			const result = await invoke('get_task_for_review', { taskId: task.id });
			const dbMessages = buildReviewMessages(result);
			// Preserve in-memory streaming messages that haven't been persisted
			// to DB yet (e.g. currently-running task output).
			const liveMessages = getTaskMessages(task.id);
			const streaming = liveMessages.filter((m) => m.streaming);
			const dbIds = new Set(dbMessages.map((m) => m.id));
			const merged = [...dbMessages, ...streaming.filter((m) => !dbIds.has(m.id))];
			setTaskMessages(task.id, merged);
			reviewTargetStore.set({ taskId: task.id, summary: task.input_text });
			await goto('/');
		} catch (e) {
			console.error('Failed to load task for review:', e);
		}
	}

	function buildReviewMessages(data) {
		const items = [];
		const msgs = data.messages || [];
		const task = data.task || {};

		const msgIds = new Set();
		for (const msg of msgs) {
			msgIds.add(msg.id);
			items.push({
				id: msg.id,
				role: msg.role,
				content: msg.content,
				type: msg.message_type === 'text' ? undefined : msg.message_type || undefined,
				voice: false,
				time: formatDate(msg.created_at),
				streaming: false,
			});
		}

		// Supplement with action/observation from steps (thoughts are already
		// part of assistant messages in session data).
		for (const step of data.steps || []) {
			if (step.action_tool) {
				const toolId = `tool-${step.id}`;
				if (!msgIds.has(toolId)) {
					const obs = (step.observation && step.observation !== '{}') ? step.observation : null;
					items.push({
						id: toolId,
						role: 'assistant',
						content: obs || `Calling ${step.action_tool}`,
						type: 'tool',
						voice: false,
						time: formatDate(step.created_at),
						streaming: false,
					});
				}
			}
		}
		items.sort((a, b) => {
			if (a.time < b.time) return -1;
			if (a.time > b.time) return 1;
			return 0;
		});
		// Fallback: if no messages or steps exist, show the task input text
		// so the review page is not completely empty.
		if (items.length === 0 && task.input_text) {
			items.push({
				id: `placeholder-${task.id}`,
				role: 'user',
				content: task.input_text,
				voice: false,
				time: formatDate(task.created_at || new Date().toISOString()),
				streaming: false,
			});
		}
		return items;
	}

	async function deleteTask(taskId) {
		try {
			await invoke('delete_task', { taskId });
			tasks = tasks.filter((t) => t.id !== taskId);
			totalCount = tasks.length;
			// If the deleted task was the active one, reset conversation state.
			const current = get(activeTaskIdStore);
			if (current === taskId) {
				activeTaskIdStore.set(null);
				clearTaskMessages(taskId);
			}
			addNotification('任务已删除', 'success', 2000);
		} catch (e) {
			addNotification(`删除失败: ${e}`, 'error', 4000);
		}
		deleteTarget = null;
	}

	async function clearHistory() {
		try {
			const count = await invoke('clear_history');
			tasks = [];
			totalCount = 0;
			hasMore = false;
			activeTaskIdStore.set(null);
			taskMessagesStore.set({});
			addNotification(`已清空 ${count} 条历史记录`, 'success', 3000);
		} catch {
			addNotification('清空历史记录失败', 'error', 4000);
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

	function toggleSelect(taskId) {
		const next = new Set(selectedIds);
		if (next.has(taskId)) {
			next.delete(taskId);
		} else {
			next.add(taskId);
		}
		selectedIds = next;
	}

	function toggleSelectAll() {
		if (selectedIds.size === tasks.length) {
			selectedIds = new Set();
		} else {
			selectedIds = new Set(tasks.map((t) => t.id));
		}
	}

	function formatDate(iso) {
		const d = new Date(iso);
		const y = d.getFullYear();
		const m = String(d.getMonth() + 1).padStart(2, '0');
		const day = String(d.getDate()).padStart(2, '0');
		const h = String(d.getHours()).padStart(2, '0');
		const min = String(d.getMinutes()).padStart(2, '0');
		const s = String(d.getSeconds()).padStart(2, '0');
		return `${y}/${m}/${day} ${h}:${min}:${s}`;
	}

	function exportSelected() {
		const selected = tasks.filter((t) => selectedIds.has(t.id));
		const json = JSON.stringify(
			{
				exported_at: new Date().toISOString(),
				count: selected.length,
				tasks: selected,
			},
			null,
			2,
		);
		const blob = new Blob([json], { type: 'application/json' });
		const url = URL.createObjectURL(blob);
		const a = document.createElement('a');
		a.href = url;
		a.download = `haven-history-${new Date().toISOString().slice(0, 10)}.json`;
		a.click();
		URL.revokeObjectURL(url);
		cancelSelectMode();
	}

	async function exportHistory() {
		try {
			const json = await invoke('export_history', {
				startDate: startDate || null,
				endDate: endDate || null,
				status: statusFilter || null,
			});
			const blob = new Blob([json], { type: 'application/json' });
			const url = URL.createObjectURL(blob);
			const a = document.createElement('a');
			a.href = url;
			a.download = `haven-history-${new Date().toISOString().slice(0, 10)}.json`;
			a.click();
			URL.revokeObjectURL(url);
		} catch {}
	}
</script>

<div class="history-page">
	<div class="header-row">
		<h1>History</h1>
		<span class="count-badge">Total {totalCount} shown</span>
		<div class="header-actions">
			{#if selectMode}
				<button
					class="md-btn md-btn--filled"
					onclick={exportSelected}
					disabled={selectedIds.size === 0}
				>
					Export Selected ({selectedIds.size})
				</button>
				<button class="md-btn md-btn--text" onclick={cancelSelectMode}>Cancel</button>
			{:else}
				<button class="md-btn md-btn--outlined" onclick={enterSelectMode}>Export</button>
				{#if tasks.length > 0}
					<button class="md-btn md-btn--danger" onclick={() => (showClearDialog = true)}>
						Clear All
					</button>
				{/if}
			{/if}
		</div>
	</div>

	<div class="filter-bar">
		<input
			class="md-input"
			type="text"
			placeholder="Search"
			bind:value={searchQuery}
			oninput={handleSearchInput}
		/>
		<div class="filter-controls">
			<MaterialSelect
				value={statusFilter}
				options={statusOptions}
				onChange={(v) => { statusFilter = v; handleFilterChange(); }}
			/>
			<button class="md-btn md-btn--outlined" onclick={() => (showDateFilter = true)}>
				{#if startDate || endDate}
					Date: {startDate ? startDate.replace(/-/g, '/') : '…'} ~ {endDate ? endDate.replace(/-/g, '/') : '…'}
				{:else}
					Date Filter
				{/if}
			</button>
		</div>
	</div>

	{#if selectMode && tasks.length > 0}
		<div class="select-bar">
			<button class="select-all-row" onclick={toggleSelectAll}>
				<div class="md-checkbox-static" class:checked={selectedIds.size === tasks.length}></div>
				<span>Select all ({tasks.length})</span>
			</button>
		</div>
	{/if}

	{#if tasks.length === 0}
		<div class="empty-state">{loading ? 'Loading...' : 'No task history yet'}</div>
	{:else}
		<div class="history-list">
			{#each tasks as task (task.id)}
				{#if selectMode}
					<button
						class="history-item history-item-btn"
						class:selected={selectedIds.has(task.id)}
						onclick={() => toggleSelect(task.id)}
					>
						<div class="history-item-inner">
							<div class="select-checkbox">
								<div class="md-checkbox-static" class:checked={selectedIds.has(task.id)}></div>
							</div>
							<div class="history-content">
								<div class="history-header">
									<span class="history-title">{task.input_text || 'Untitled'}</span>
									<MaterialBadge variant={statusVariant(task.status)} text={task.status} />
								</div>
								<div class="history-meta">
									{#if task.classification}
										<span>{task.classification}</span>
									{/if}
									<span>{formatDate(task.created_at)}</span>
									{#if task.session_id}
										<span class="session-id" title={task.session_id}>
											Session: {task.session_id.slice(0, 8)}...
										</span>
									{/if}
								</div>
								{#if task.transcript}
									<div class="history-transcript">"{task.transcript}"</div>
								{/if}
							</div>
						</div>
					</button>
				{:else}
					<div class="history-item">
						<div class="history-item-inner">
							<div class="history-content">
								<div class="history-header">
									<span class="history-title">{task.input_text || 'Untitled'}</span>
									<MaterialBadge variant={statusVariant(task.status)} text={task.status} />
								</div>
								<div class="history-meta">
									{#if task.classification}
										<span>{task.classification}</span>
									{/if}
									<span>{formatDate(task.created_at)}</span>
									{#if task.session_id}
										<span class="session-id" title={task.session_id}>
											Session: {task.session_id.slice(0, 8)}...
										</span>
									{/if}
								</div>
								{#if task.transcript}
									<div class="history-transcript">"{task.transcript}"</div>
								{/if}
							</div>
						</div>
						<div class="history-actions">
							<button class="md-btn md-btn--xs md-btn--tonal" onclick={() => reviewTask(task)}>
								Review
							</button>
							<button
								class="md-btn md-btn--xs md-btn--danger"
								onclick={() => (deleteTarget = task)}
							>
								Delete
							</button>
						</div>
					</div>
				{/if}
			{/each}
		</div>

		{#if hasMore}
			<div class="load-more-row">
				<button class="md-btn md-btn--outlined" onclick={loadMore} disabled={loading}>
					{loading ? 'Loading...' : 'Load More'}
				</button>
			</div>
		{/if}
	{/if}
</div>

<MaterialDialog
	open={showDateFilter}
	onClose={() => (showDateFilter = false)}
	title="Date Filter"
>
	{#snippet children()}
		<div class="date-filter-dialog">
			<div class="date-range-header">
				<span class="date-range-label">Selected range</span>
				<span class="date-range-value">
					{#if startDate || endDate}
						{startDate ? startDate.replace(/-/g, '/') : '…'} — {endDate ? endDate.replace(/-/g, '/') : '…'}
					{:else}
						All dates
					{/if}
				</span>
			</div>
			<div class="date-input-row">
				<div class="date-field">
					<label class="date-filter-label" for="start-date">Start date</label>
					<MaterialDatePicker
						id="start-date"
						value={startDate}
						max={todayISO}
						onChange={(v) => {
							startDate = v;
							if (endDate && endDate < startDate) endDate = '';
							handleFilterChange();
						}}
					/>
				</div>
				<div class="date-field">
					<label class="date-filter-label" for="end-date">End date</label>
					<MaterialDatePicker
						id="end-date"
						value={endDate}
						min={startDate || undefined}
						max={todayISO}
						onChange={(v) => { endDate = v; handleFilterChange(); }}
					/>
				</div>
			</div>
		</div>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => { startDate = ''; endDate = ''; handleFilterChange(); }}>
			Clear
		</button>
		<button class="md-btn md-btn--filled" onclick={() => (showDateFilter = false)}>
			Done
		</button>
	{/snippet}
</MaterialDialog>

<MaterialDialog
	open={deleteTarget !== null}
	onClose={() => (deleteTarget = null)}
	title="Delete Task"
>
	{#snippet children()}
		<p class="dialog-text">
			Delete "{deleteTarget?.input_text || 'Untitled'}"? This action cannot be undone.
		</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => (deleteTarget = null)}>Cancel</button>
		<button class="md-btn md-btn--danger" onclick={() => deleteTask(deleteTarget.id)}>
			Delete
		</button>
	{/snippet}
</MaterialDialog>

<MaterialDialog
	open={showClearDialog}
	onClose={() => (showClearDialog = false)}
	title="Clear All History"
>
	{#snippet children()}
		<p class="dialog-text">
			This will permanently delete all task history. This action cannot be undone.
		</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => (showClearDialog = false)}>Cancel</button>
		<button class="md-btn md-btn--danger" onclick={clearHistory}>Clear All</button>
	{/snippet}
</MaterialDialog>

<style>
	.history-page {
		max-width: 1000px;
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
	.history-item {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-sm);
		transition: box-shadow var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.history-item:hover {
		box-shadow: var(--md-sys-elevation-1);
	}
	.history-item.selected {
		background: var(--md-sys-color-primary-container);
		border-color: var(--md-sys-color-primary);
	}
	.history-item-inner {
		display: flex;
		gap: var(--md-sys-space-md);
		align-items: flex-start;
	}
	.history-item-btn {
		width: 100%;
		text-align: left;
		font-family: inherit;
		cursor: pointer;
	}
	.history-item-btn:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: -2px;
	}
	.select-checkbox {
		flex-shrink: 0;
		display: flex;
		align-items: center;
		height: 100%;
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
	.history-content {
		flex: 1;
		min-width: 0;
	}
	.history-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
	}
	.history-title {
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
		font-size: 14px;
	}
	.history-meta {
		display: flex;
		gap: var(--md-sys-space-lg);
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-sm);
	}
	.session-id {
		opacity: 0.6;
	}
	.history-transcript {
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		background: var(--md-sys-color-surface-container);
		border-radius: var(--md-sys-shape-extra-small);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.history-actions {
		display: flex;
		justify-content: space-between;
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
</style>