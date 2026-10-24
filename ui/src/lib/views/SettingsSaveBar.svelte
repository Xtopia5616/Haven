<script lang="ts">
	import SettingsErrorNotice from '$lib/SettingsErrorNotice.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import type { SettingsFailurePresentation } from '$lib/settingsSaveFailure.ts';

	type SaveState = 'idle' | 'saving' | 'saved' | 'error';
	interface Props {
		dirty: boolean;
		dirtySectionLabels: string[];
		saveState: SaveState;
		saveError: SettingsFailurePresentation | null;
		blocked?: boolean;
		onDiscard: () => void;
		onSave: () => Promise<void>;
	}

	let {
		dirty,
		dirtySectionLabels,
		saveState,
		saveError,
		blocked = false,
		onDiscard,
		onSave,
	}: Props = $props();
</script>

<div class="save-bar">
	{#if saveState === 'error' && saveError}
		<SettingsErrorNotice failure={saveError} />
	{/if}
	{#if dirty}
		<div class="save-summary motion-surface-enter" aria-live="polite">
			<strong>有未保存更改</strong>
			<span>{dirtySectionLabels.length ? dirtySectionLabels.join('、') : '设置'}</span>
		</div>
	{/if}
	<div class="save-actions">
		{#if dirty}
			<MaterialButton
				variant="text"
				width="compact"
				className="save-action-btn"
				label="放弃"
				onclick={onDiscard}
				disabled={saveState === 'saving' || blocked}
			/>
		{/if}
		<div class="save-button-status" aria-live="polite" aria-busy={saveState === 'saving'}>
			<MaterialButton
				variant="outlined"
				width="compact"
				className="save-action-btn save-btn--dirty"
				label={saveState === 'saving' ? '保存中…' : dirty ? '保存更改' : '保存'}
				ariaLabel={saveState === 'saving' ? '正在保存设置' : '保存设置更改'}
				onclick={onSave}
				disabled={!dirty || saveState === 'saving' || blocked}
			/>
		</div>
	</div>
</div>

<style>
	.save-bar {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm) var(--md-comp-toolbar-gap);
		max-width: 100%;
		min-width: 0;
	}
	:global(.settings-error) {
		flex: 1 1 100%;
	}
	.save-summary {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-2xs);
		margin-right: auto;
		min-width: 0;
		max-width: min(28rem, 42vw);
	}
	.save-summary strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.save-summary span {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		overflow-wrap: anywhere;
	}
	:global(.save-action-btn) {
		white-space: nowrap;
	}
	:global(.save-btn--dirty.md-btn--outlined) {
		--_btn-bg: var(--md-sys-color-surface-container-high);
		--_btn-fg: var(--md-sys-color-on-surface);
		--_btn-border: 1px solid var(--md-sys-color-outline-variant);
		--_btn-state: var(--md-sys-color-on-surface);
	}
	:global(.save-btn--dirty.md-btn--outlined:enabled:hover) {
		--_btn-bg: var(--md-sys-color-surface-container-highest);
	}
	.save-button-status {
		display: flex;
		flex: 0 0 auto;
		min-width: 0;
	}
	.save-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 0 0 auto;
	}
	@media screen and (max-width: 520px) {
		.save-summary {
			flex: 1 1 100%;
			max-width: none;
		}
		.save-actions {
			margin-left: auto;
		}
	}
</style>
