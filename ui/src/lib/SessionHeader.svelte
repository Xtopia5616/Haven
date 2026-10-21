<script lang="ts">
	import type { Snippet } from 'svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';
	import MaterialSelect from './MaterialSelect.svelte';
	import { sameExecutionWorkspacePath } from './executionWorkspaceStore.ts';
	import type { SelectOption } from './selectOption.ts';

	interface Props {
		title?: string;
		hasSession?: boolean;
		canEndSession?: boolean;
		onNew?: () => void;
		onDelete?: () => void;
		onEnd?: () => void;
		executionWorkspaceRoot?: string | null;
		executionWorkspaceSwitching?: boolean;
		savedExecutionWorkspaces?: string[];
		onSelectExecutionWorkspace?: (root: string | null) => void;
		onAddExecutionWorkspace?: () => void;
		onRemoveExecutionWorkspace?: (root: string) => void;
		children?: Snippet;
	}

	/**
	 * SessionHeader — keeps the active session identity and lifecycle
	 * state visible above the message timeline.
	 */
	let {
		title = '新会话',
		hasSession = false,
		canEndSession = true,
		onNew,
		onDelete,
		onEnd,
		executionWorkspaceRoot = null,
		executionWorkspaceSwitching = false,
		savedExecutionWorkspaces = [],
		onSelectExecutionWorkspace,
		onAddExecutionWorkspace,
		onRemoveExecutionWorkspace,
		children,
	}: Props = $props();
	let executionWorkspaceOptions = $derived.by(() => {
		const options: SelectOption[] = [{ value: '', label: '自动检测', group: '执行工作区' }];
		if (
			executionWorkspaceRoot &&
			!savedExecutionWorkspaces.some((root) =>
				sameExecutionWorkspacePath(root, executionWorkspaceRoot),
			)
		) {
			options.push({
				value: executionWorkspaceRoot,
				label: `当前：${executionWorkspaceRoot}`,
				group: '当前工作区',
			});
		}
		options.push(
			...savedExecutionWorkspaces.map((root) => ({
				value: root,
				label: root,
				group: '已保存工作区',
			})),
		);
		return options;
	});
	let selectedWorkspaceIsSaved = $derived(
		!!executionWorkspaceRoot &&
			savedExecutionWorkspaces.some((root) =>
				sameExecutionWorkspacePath(root, executionWorkspaceRoot),
			),
	);
	let selectedWorkspaceValue = $derived(
		executionWorkspaceRoot
			? (savedExecutionWorkspaces.find((root) =>
					sameExecutionWorkspacePath(root, executionWorkspaceRoot),
				) ?? executionWorkspaceRoot)
			: '',
	);
</script>

<header class="session-header">
	<div class="session-header__identity">
		<div class="session-header__title-row">
			<h1>{title}</h1>
			<div class="session-header__workspace-controls" role="group" aria-label="执行工作区">
				<MaterialSelect
					id="session-execution-workspace"
					ariaLabel={`执行工作区：${selectedWorkspaceValue || '自动检测'}`}
					width="compact"
					value={selectedWorkspaceValue}
					options={executionWorkspaceOptions}
					iconOnly
					icon="folder"
					title={`执行工作区：${selectedWorkspaceValue || '自动检测'}`}
					disabled={executionWorkspaceSwitching}
					onChange={(root) => onSelectExecutionWorkspace?.(root || null)}
				/>
				<MaterialIconButton
					size="toolbar"
					icon="delete"
					variant="danger-text"
					label={`从工作区快捷列表移除 ${executionWorkspaceRoot || '当前工作区'}`}
					title="仅移除工作区快捷项，不会删除磁盘目录"
					disabled={executionWorkspaceSwitching || !selectedWorkspaceIsSaved}
					onclick={() => {
						if (executionWorkspaceRoot) onRemoveExecutionWorkspace?.(executionWorkspaceRoot);
					}}
				/>
				<MaterialIconButton
					size="toolbar"
					icon="plus"
					className="session-header__workspace-add"
					label="选择目录并保存为工作区"
					ariaBusy={executionWorkspaceSwitching}
					disabled={executionWorkspaceSwitching || !onAddExecutionWorkspace}
					title="选择目录并保存为工作区"
					onclick={() => onAddExecutionWorkspace?.()}
				/>
			</div>
		</div>
	</div>
	<div class="session-header__actions">
		<div class="session-header__toolbar">
			{@render children?.()}
		</div>
		<div class="session-header__lifecycle">
			<MaterialIconButton
				size="toolbar"
				icon="plus"
				variant="primary"
				className="session-header__new"
				label="新建会话"
				title="新建会话"
				onclick={() => onNew?.()}
			/>
			{#if hasSession}
				<MaterialIconButton
					size="toolbar"
					icon="delete"
					variant="danger-outline"
					className="session-header__delete"
					label="删除会话"
					title="删除会话"
					onclick={() => onDelete?.()}
				/>
				{#if canEndSession}
					<MaterialIconButton
						size="toolbar"
						icon="check"
						variant="success-outline"
						className="session-header__end"
						label="完成会话"
						title="完成会话"
						onclick={() => onEnd?.()}
					/>
				{/if}
			{/if}
		</div>
	</div>
</header>

<style>
	.session-header {
		position: relative;
		z-index: 4;
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--md-sys-space-lg);
		padding: var(--md-sys-space-md) var(--md-sys-space-2xl);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		background: color-mix(
			in srgb,
			var(--md-sys-color-surface-container-low) 92%,
			var(--md-sys-color-primary) 8%
		);
		min-width: 0;
	}
	.session-header::before {
		content: '';
		position: absolute;
		inset: 0 auto 0 0;
		width: 3px;
		background: var(--md-sys-color-primary);
		opacity: 0.72;
	}
	.session-header__identity {
		min-width: 0;
		display: flex;
		align-items: center;
	}
	.session-header__title-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.session-header h1 {
		min-width: 0;
		max-width: min(52vw, 520px);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 700;
		letter-spacing: 0;
		line-height: var(--md-sys-typescale-title-large-line-height);
	}
	.session-header__workspace-controls {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
	}
	:global(.session-header__workspace-controls .md-select-container[data-icon-only='true']) {
		flex: 0 0 auto;
		width: auto;
	}
	.session-header__actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex-shrink: 0;
		min-width: 0;
	}
	.session-header__toolbar,
	.session-header__lifecycle {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
	}
	.session-header__lifecycle {
		flex-shrink: 0;
	}
	@media (max-width: 640px) {
		.session-header {
			grid-template-columns: minmax(0, 1fr);
			gap: var(--md-sys-space-sm);
			padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		}
		.session-header__title-row {
			align-items: center;
			flex-direction: row;
			flex-wrap: wrap;
			gap: var(--md-sys-space-sm);
		}
		.session-header h1 {
			max-width: min(92vw, 520px);
			font-size: var(--md-sys-typescale-title-medium-size);
		}
		.session-header__actions {
			flex-wrap: wrap;
			justify-content: flex-start;
			gap: var(--md-sys-space-xs) var(--md-sys-space-sm);
			padding-top: var(--md-sys-space-sm);
			border-top: 1px solid var(--md-sys-color-outline-variant);
		}
		.session-header__toolbar {
			flex: 1 1 100%;
		}
		.session-header__lifecycle {
			display: flex;
			width: auto;
			gap: var(--md-sys-space-xs);
		}
	}
	@media screen and (min-width: 641px) and (max-width: 1024px),
		screen and (orientation: landscape) and (max-height: 560px) and (min-width: 560px) and (max-width: 1024px) {
		.session-header {
			grid-template-columns: minmax(0, 1fr);
			gap: var(--md-sys-space-xs);
			padding: var(--md-sys-space-xs) var(--md-sys-space-md);
		}
		.session-header__title-row {
			flex-wrap: nowrap;
			gap: var(--md-sys-space-sm);
		}
		.session-header h1 {
			max-width: 28vw;
			font-size: var(--md-sys-typescale-title-medium-size);
		}
		.session-header__workspace-controls {
			flex: 0 1 auto;
		}
		:global(.session-header__workspace-controls .md-select-container[data-icon-only='true']) {
			flex-basis: auto;
			min-width: 0;
		}
		.session-header__actions {
			flex-wrap: nowrap;
			justify-content: space-between;
			gap: var(--md-sys-space-sm);
			padding-top: 0;
			border-top: 0;
		}
		.session-header__toolbar {
			flex: 1 1 auto;
		}
		.session-header__lifecycle {
			width: auto;
			display: flex;
			gap: var(--md-sys-space-xs);
		}
	}
</style>
