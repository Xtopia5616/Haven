<script>
	import { untrack } from 'svelte';
	import JsonView from './JsonView.svelte';

	// Recursive, collapsible JSON tree viewer with syntax coloring and a
	// copy-to-clipboard button at the root. Used by ToolResultCard for tool
	// observations whose JSON has no dedicated renderer, so both live chat
	// and history review share the same visualization.

	let {
		value = null,
		key = '',
		indexed = false,
		depth = 0,
		defaultDepth = 2,
		copyable = true,
	} = $props();

	let isArray = $derived(Array.isArray(value));
	let isContainer = $derived(isArray || (value !== null && typeof value === 'object'));
	let count = $derived(isArray ? value.length : isContainer ? Object.keys(value).length : 0);

	// Root always starts expanded; nested containers expand until
	// `defaultDepth` so deep payloads don't explode on first paint. Captured
	// once at init — each node is keyed by its JSON path, so props never
	// change for a live node.
	let expanded = $state(untrack(() => depth === 0 || depth < defaultDepth));
	let copied = $state(false);
	let copyTimer = null;

	function summaryOf() {
		if (count === 0) return isArray ? '[ ]' : '{ }';
		return isArray ? `[ ${count} 项 ]` : `{ ${count} 个键 }`;
	}

	function keyLabel(k) {
		return indexed ? k : JSON.stringify(k);
	}

	/** @param {unknown} v */
	function valInfo(v) {
		if (v === null) return { cls: 'jv-null', text: 'null' };
		const t = typeof v;
		if (t === 'boolean') return { cls: 'jv-bool', text: String(v) };
		if (t === 'number') return { cls: 'jv-num', text: String(v) };
		if (t === 'string') {
			const full = JSON.stringify(v);
			return { cls: 'jv-str', text: full.length > 160 ? `${full.slice(0, 157)}…"` : full, full };
		}
		return { cls: '', text: String(v), full: String(v) };
	}

	function toggle() {
		if (count > 0) expanded = !expanded;
	}

	async function copyJson() {
		try {
			await navigator.clipboard.writeText(JSON.stringify(value, null, 2));
			copied = true;
			clearTimeout(copyTimer);
			copyTimer = setTimeout(() => (copied = false), 1500);
		} catch {
			// Clipboard unavailable (e.g. non-secure context) — ignore.
		}
	}
</script>

<div class="jv-view" class:jv-root-view={depth === 0}>
	{#if depth === 0 && copyable}
		<div class="jv-toolbar">
			<button class="jv-copy" class:jv-copied={copied} type="button" onclick={copyJson} aria-label="复制 JSON">
				{#if copied}
					<span aria-hidden="true">✓</span>已复制
				{:else}
					<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
					复制
				{/if}
			</button>
		</div>
	{/if}
	<div class="jv-body" class:jv-root-body={depth === 0}>
		{#if isContainer}
			<button
				class="jv-row jv-container"
				class:jv-empty={count === 0}
				type="button"
				disabled={count === 0}
				aria-expanded={count > 0 ? expanded : undefined}
				onclick={toggle}
			>
				<span class="jv-caret" aria-hidden="true">{expanded ? '▾' : '▸'}</span>
				{#if key}
					<span class="jv-key">{keyLabel(key)}</span><span class="jv-punct">:&nbsp;</span>
				{/if}
				{#if expanded}
					<span class="jv-punct">{isArray ? '[' : '{'}</span>
					{#if count === 0}
						<span class="jv-punct">{isArray ? ']' : '}'}</span>
					{/if}
				{:else}
					<span class="jv-summary">{summaryOf()}</span>
				{/if}
			</button>
			{#if expanded && count > 0}
				<div class="jv-children">
					{#if isArray}
						{#each value as item, i (i)}
							<JsonView value={item} key={String(i)} indexed depth={depth + 1} {defaultDepth} copyable={false} />
						{/each}
					{:else}
						{#each Object.entries(value) as [k, v] (k)}
							<JsonView value={v} key={k} depth={depth + 1} {defaultDepth} copyable={false} />
						{/each}
					{/if}
				</div>
			{/if}
		{:else}
			{@const info = valInfo(value)}
			<div class="jv-row">
				{#if key}
					<span class="jv-key">{keyLabel(key)}</span><span class="jv-punct">:&nbsp;</span>
				{/if}
				{#if info.cls}
					<span class="jv-value {info.cls}" title={info.full ?? ''}>{info.text}</span>
				{:else}
					<span title={info.full ?? ''}>{info.text}</span>
				{/if}
			</div>
		{/if}
	</div>
</div>

<style>
	.jv-root-view {
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		overflow: hidden;
	}
	.jv-toolbar {
		display: flex;
		justify-content: flex-end;
		padding: 4px var(--md-sys-space-xs) 0;
	}
	.jv-copy {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: 10px;
		font-weight: 600;
		font-family: var(--md-sys-typescale-body);
		color: var(--md-sys-color-on-surface-variant);
		background: transparent;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		padding: 2px 8px;
		cursor: pointer;
		transition: background-color 0.15s ease, color 0.15s ease;
	}
	.jv-copy:hover {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface);
	}
	.jv-copy.jv-copied {
		color: var(--md-sys-color-success);
	}
	.jv-body {
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		line-height: 1.6;
		color: var(--md-sys-color-on-surface-variant);
	}
	.jv-root-body {
		max-height: 240px;
		overflow: auto;
		padding: var(--md-sys-space-2xs) var(--md-sys-space-sm) var(--md-sys-space-sm);
	}
	.jv-row {
		white-space: pre-wrap;
		word-break: break-word;
	}
	.jv-container {
		display: block;
		width: 100%;
		text-align: left;
		font-family: inherit;
		font-size: inherit;
		line-height: inherit;
		color: inherit;
		background: none;
		border: none;
		padding: 0 var(--md-sys-space-2xs);
		margin: 0 calc(-1 * var(--md-sys-space-2xs));
		border-radius: 4px;
		cursor: pointer;
		user-select: none;
	}
	.jv-container:not(:disabled):hover {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 5%, transparent);
	}
	.jv-container:disabled {
		cursor: default;
	}
	.jv-caret {
		display: inline-block;
		width: 12px;
		font-size: 9px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.jv-empty .jv-caret {
		opacity: 0.35;
	}
	.jv-children {
		margin-left: 12px;
		padding-left: 6px;
		border-left: 1px solid var(--md-sys-color-outline-variant);
	}
	.jv-key {
		color: var(--md-sys-color-tertiary);
		font-weight: 600;
	}
	.jv-punct {
		color: var(--md-sys-color-on-surface-variant);
	}
	.jv-summary {
		font-style: italic;
		opacity: 0.85;
	}
	.jv-value {
		font-family: var(--md-sys-typescale-mono);
	}
	.jv-str {
		color: var(--md-sys-color-success);
	}
	.jv-num {
		color: var(--md-sys-color-tertiary);
	}
	.jv-bool {
		color: var(--md-sys-color-primary);
	}
	.jv-null {
		font-style: italic;
	}
</style>
