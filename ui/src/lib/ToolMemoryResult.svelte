<script>
	import JsonView from '$lib/JsonView.svelte';

	let { data = {} } = $props();
	let facts = $derived(Array.isArray(data.facts) ? data.facts : []);
	let hits = $derived(Array.isArray(data.hits) ? data.hits : []);

	/** @param {unknown} value */
	function scoreLabel(value) {
		const score = Number(value);
		return Number.isFinite(score) ? score.toFixed(2) : '—';
	}
</script>

{#if Array.isArray(data.facts)}
	<div class="tool-card-count">{facts.length} 条记忆事实</div>
	{#if facts.length > 0}
		<div class="memory-list">
			{#each facts as fact, index (`${fact.id ?? `${fact.subject ?? ''}:${fact.predicate ?? ''}:${index}`}`)}
				<div class="memory-row">
					<div class="memory-triple">
						<span class="memory-subject">{fact.subject || '用户'}</span>
						<span class="memory-predicate">{fact.predicate || '事实'}</span>
						<span class="memory-object" title={fact.object}>{fact.object || '—'}</span>
					</div>
					<div class="memory-meta">
						{#if fact.confidence != null}<span>置信度 {scoreLabel(fact.confidence)}</span>{/if}
						{#if Array.isArray(fact.tags) && fact.tags.length > 0}<span>{fact.tags.join(' · ')}</span>{/if}
					</div>
					{#if fact.source_snippet}<div class="memory-snippet">{fact.source_snippet}</div>{/if}
				</div>
			{/each}
		</div>
	{:else}
		<p class="tool-card-empty">没有找到记忆事实</p>
	{/if}
{:else if Array.isArray(data.hits)}
	<div class="tool-card-count">{hits.length} 条召回结果{data.mode ? ` · ${data.mode}` : ''}</div>
	{#if hits.length > 0}
		<div class="memory-list">
			{#each hits as hit, index (hit.entity_id ?? index)}
				<div class="memory-row">
					<div class="memory-hit-text">{hit.text || '—'}</div>
					<div class="memory-meta">
						{#if hit.score != null}<span>相关度 {scoreLabel(hit.score)}</span>{/if}
						{#if hit.model}<span>{hit.model}</span>{/if}
					</div>
				</div>
			{/each}
		</div>
	{:else}
		<p class="tool-card-empty">没有找到相关记忆</p>
	{/if}
{:else if data.operation === 'remember' && data.stored}
	<div class="memory-action"><span class="memory-badge">已记住</span><span>{data.stored.predicate}: {data.stored.object}</span></div>
{:else if data.operation === 'forget'}
	<div class="memory-action"><span class="memory-badge">已删除</span><span>{data.deleted ?? 0} 条事实</span></div>
{:else}
	<div class="tool-card-meta">记忆操作：{data.operation || '结果'}</div>
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.tool-card-count,
	.tool-card-meta,
	.tool-card-empty {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-count { font-weight: 600; margin-bottom: var(--md-sys-space-xs); }
	.tool-card-meta { margin-bottom: var(--md-sys-space-xs); }
	.tool-card-empty { margin: 0; }
	.memory-list { max-height: 240px; overflow-y: auto; border-radius: var(--md-sys-shape-extra-small); }
	.memory-row { padding: var(--md-sys-space-2xs) var(--md-sys-space-xs); border-radius: 4px; }
	.memory-row:nth-child(odd) { background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent); }
	.memory-triple { display: flex; align-items: baseline; gap: var(--md-sys-space-2xs); min-width: 0; }
	.memory-subject,
	.memory-predicate { flex: none; color: var(--md-sys-color-on-surface-variant); font-size: var(--md-sys-typescale-label-small-size); }
	.memory-predicate::before { content: '·'; margin-right: var(--md-sys-space-2xs); }
	.memory-object,
	.memory-hit-text { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--md-sys-color-on-surface); font-family: var(--md-sys-typescale-mono); font-size: var(--md-sys-typescale-code-size); }
	.memory-hit-text { white-space: pre-wrap; word-break: break-word; }
	.memory-meta { display: flex; gap: var(--md-sys-space-xs); margin-top: 2px; color: var(--md-sys-color-on-surface-variant); font-size: var(--md-sys-typescale-label-small-size); }
	.memory-snippet { margin-top: 2px; color: var(--md-sys-color-on-surface-variant); font-size: var(--md-sys-typescale-label-small-size); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.memory-action { display: flex; align-items: baseline; gap: var(--md-sys-space-xs); color: var(--md-sys-color-on-surface); }
	.memory-badge { flex: none; padding: 1px 6px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font-size: var(--md-sys-typescale-label-small-size); font-weight: 700; }
</style>
