<script>
	import { untrack } from 'svelte';
	import JsonView from './JsonView.svelte';
	import logger from './logger.ts';
	import { formatError } from './formatError.ts';

	// Recursive, collapsible JSON tree viewer with syntax coloring and a
	// copy-to-clipboard button at the root. Used by ToolResultCard for tool
	// observations whose JSON has no dedicated renderer, so both live chat
	// and history resume share the same visualization.

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
	/** @type {ReturnType<typeof setTimeout> | null} */
	let copyTimer = null;

	function summaryOf() {
		if (count === 0) return isArray ? '[ ]' : '{ }';
		return isArray ? `[ ${count} 项 ]` : `{ ${count} 个键 }`;
	}

	/** @param {string} k */
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
			if (copyTimer) clearTimeout(copyTimer);
			copyTimer = setTimeout(() => (copied = false), 1500);
		} catch (error) {
			logger.warn('JsonView', 'JSON copy failed', formatError(error));
		}
	}
</script>

<div class="jv-view" class:jv-root-view={depth === 0}>
	{#if depth === 0 && copyable}
		<div class="jv-toolbar md-toolbar">
			<span class="jv-root-kind">
				{#if isArray}
					数组 · {count}
				{:else if isContainer}
					对象 · {count}
				{:else}
					JSON
				{/if}
			</span>
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
				class:jv-open={expanded}
				type="button"
				disabled={count === 0}
				aria-expanded={count > 0 ? expanded : undefined}
				onclick={toggle}
			>
				<span class="jv-caret" aria-hidden="true">
					<svg
						width="12"
						height="12"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2.5"
						stroke-linecap="round"
						stroke-linejoin="round"
						><polyline points="6 9 12 15 18 9" /></svg
					>
				</span>
				{#if key}
					<span class={indexed ? 'jv-index' : 'jv-key'}>{keyLabel(key)}</span><span class="jv-punct">:&nbsp;</span>
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
				<div class="jv-row jv-close" aria-hidden="true">
					<span class="jv-caret-spacer"></span>
					<span class="jv-punct">{isArray ? ']' : '}'}</span>
				</div>
			{/if}
		{:else}
			{@const info = valInfo(value)}
			<div class="jv-row jv-leaf">
				<span class="jv-caret-spacer"></span>
				{#if key}
					<span class={indexed ? 'jv-index' : 'jv-key'}>{keyLabel(key)}</span><span class="jv-punct">:&nbsp;</span>
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
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-xs);
		padding: 2px var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.jv-root-kind {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		font-family: var(--md-sys-typescale-mono);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		padding: 0 4px;
	}
	.jv-copy {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		font-family: var(--md-sys-typescale-body);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		background: transparent;
		border: 1px solid transparent;
		border-radius: var(--md-sys-shape-full);
		padding: 1px 8px;
		cursor: pointer;
		transition:
			background-color 0.15s ease,
			color 0.15s ease,
			border-color 0.15s ease;
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
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.jv-root-body {
		max-height: 240px;
		overflow: auto;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm) var(--md-sys-space-sm);
		scrollbar-width: thin;
		scrollbar-color: var(--md-sys-color-outline-variant) transparent;
	}
	.jv-root-body::-webkit-scrollbar {
		width: 4px;
		height: 4px;
	}
	.jv-root-body::-webkit-scrollbar-thumb {
		background: var(--md-sys-color-outline-variant);
		border-radius: 2px;
	}
	.jv-row {
		display: flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: 0 2px;
		white-space: pre-wrap;
		word-break: break-word;
		border-radius: 4px;
		padding: 1px var(--md-sys-space-2xs);
		margin: 0 calc(-1 * var(--md-sys-space-2xs));
	}
	.jv-container {
		width: calc(100% + 2 * var(--md-sys-space-2xs));
		text-align: left;
		font-family: inherit;
		font-size: inherit;
		line-height: inherit;
		color: inherit;
		background: none;
		border: none;
		cursor: pointer;
		user-select: none;
	}
	.jv-container:not(:disabled):hover,
	.jv-leaf:hover {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 5%, transparent);
	}
	.jv-container:disabled {
		cursor: default;
	}
	.jv-caret,
	.jv-caret-spacer {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 12px;
		height: 12px;
		flex: none;
		align-self: center;
		color: var(--md-sys-color-on-surface-variant);
		transition: transform 0.15s ease;
	}
	.jv-container:not(.jv-open) .jv-caret {
		transform: rotate(-90deg);
	}
	.jv-empty .jv-caret {
		opacity: 0.35;
	}
	.jv-children {
		margin: 0 0 0 6px;
		padding: 0 0 0 10px;
		border-left: 1px solid color-mix(in srgb, var(--md-sys-color-outline-variant) 80%, transparent);
	}
	.jv-close {
		color: var(--md-sys-color-on-surface-variant);
	}
	.jv-key {
		color: var(--md-sys-color-tertiary);
		font-weight: 600;
	}
	.jv-index {
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 500;
		font-variant-numeric: tabular-nums;
		opacity: 0.72;
		min-width: 1.25em;
	}
	.jv-punct {
		color: color-mix(in srgb, var(--md-sys-color-on-surface-variant) 70%, transparent);
	}
	.jv-summary {
		font-style: normal;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		padding: 0 6px;
		border-radius: var(--md-sys-shape-full);
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
		color: var(--md-sys-color-on-surface-variant);
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
		font-weight: 600;
	}
	.jv-null {
		font-style: italic;
		opacity: 0.65;
	}
</style>
