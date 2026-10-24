<script lang="ts">
	import type { Snippet } from 'svelte';
	import MaterialButton from './MaterialButton.svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';
	import MaterialSelect from './MaterialSelect.svelte';
	import { sameExecutionWorkspacePath } from './executionWorkspaceStore.ts';
	import type { SelectOption } from './selectOption.ts';
	import type { ExecutionMode } from './contracts/generatedCommands.ts';

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
		executionMode?: ExecutionMode;
		executionModeSwitching?: boolean;
		onSelectExecutionMode?: (mode: ExecutionMode) => void;
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
		executionMode = 'collaborative',
		executionModeSwitching = false,
		onSelectExecutionMode,
		onAddExecutionWorkspace,
		onRemoveExecutionWorkspace,
		children,
	}: Props = $props();
	let executionWorkspaceOptions = $derived.by(() => {
		const options: SelectOption[] = [{ value: '', label: '新会话默认目录', group: '工作目录' }];
		if (
			executionWorkspaceRoot &&
			!savedExecutionWorkspaces.some((root) =>
				sameExecutionWorkspacePath(root, executionWorkspaceRoot),
			)
		) {
			options.push({
				value: executionWorkspaceRoot,
				label: `当前：${executionWorkspaceRoot}`,
				group: '当前会话目录',
			});
		}
		options.push(
			...savedExecutionWorkspaces.map((root) => ({
				value: root,
				label: root,
				group: '快捷目录',
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
	const executionModeOptions: SelectOption[] = [
		{ value: 'analyze', label: '只分析', group: '执行方式' },
		{ value: 'collaborative', label: '协作执行', group: '执行方式' },
		{ value: 'unattended', label: '无人值守', group: '执行方式' },
	];
	const executionModeLabels: Record<ExecutionMode, string> = {
		analyze: '只分析',
		collaborative: '协作执行',
		unattended: '无人值守',
	};
</script>

<header class="session-header">
	<div class="session-header__identity">
		<div class="session-header__title-row">
			<h1 {title}>{title}</h1>
			<div class="session-header__workspace-controls" role="group" aria-label="会话执行上下文">
				<MaterialSelect
					id="session-execution-workspace"
					ariaLabel="会话工作目录"
					width="standard"
					value={selectedWorkspaceValue}
					options={executionWorkspaceOptions}
					title={`工作目录：${selectedWorkspaceValue || '新会话默认目录'}`}
					disabled={executionWorkspaceSwitching}
					onChange={(root) => onSelectExecutionWorkspace?.(root || null)}
				/>
				<MaterialIconButton
					size="toolbar"
					icon="delete"
					variant="danger-text"
					label={`从快捷列表移除 ${executionWorkspaceRoot || '当前工作目录'}`}
					title="仅移除快捷项，不会删除磁盘目录"
					disabled={executionWorkspaceSwitching || !selectedWorkspaceIsSaved}
					onclick={() => {
						if (executionWorkspaceRoot)
							onRemoveExecutionWorkspace?.(executionWorkspaceRoot);
					}}
				/>
				<MaterialIconButton
					size="toolbar"
					icon="plus"
					className="session-header__workspace-add"
					label="选择目录并保存为快捷项"
					ariaBusy={executionWorkspaceSwitching}
					disabled={executionWorkspaceSwitching || !onAddExecutionWorkspace}
					title="选择当前会话的工作目录，并保存快捷项"
					onclick={() => onAddExecutionWorkspace?.()}
				/>
			<MaterialSelect
				id="session-execution-mode"
				ariaLabel="会话执行方式"
				width="standard"
				value={executionMode}
				options={executionModeOptions}
				title={`执行方式：${executionModeLabels[executionMode]}`}
				disabled={executionModeSwitching || !onSelectExecutionMode}
				onChange={(mode) => {
					if (mode === 'analyze' || mode === 'collaborative' || mode === 'unattended')
						onSelectExecutionMode?.(mode);
				}}
			/>
			</div>
		</div>
	</div>
	<div class="session-header__actions">
		<div class="session-header__toolbar">
			{@render children?.()}
		</div>
		<div class="session-header__lifecycle">
			<MaterialButton
				width="compact"
				variant="filled"
				className="session-header__new"
				label="新建会话"
				title="新建会话"
				onclick={() => onNew?.()}
			/>
			{#if hasSession}
				<MaterialButton
					width="compact"
					variant="danger"
					className="session-header__delete"
					label="删除会话"
					title="删除会话"
					onclick={() => onDelete?.()}
				/>
				{#if canEndSession}
					<MaterialButton
						width="compact"
						variant="success"
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
		padding: var(--md-sys-space-sm) var(--chat-inline-gutter, var(--md-sys-space-2xl));
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container-low);
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
		width: 100%;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.session-header h1 {
		flex: 1;
		min-width: 0;
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--chat-title-size, var(--md-sys-typescale-title-large-size));
		font-weight: 600;
		letter-spacing: 0;
		line-height: var(--md-sys-typescale-title-large-line-height);
	}
	.session-header__workspace-controls {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
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
	.session-header__lifecycle :global(.md-btn) {
		width: auto;
		min-width: 0;
	}
	@container conversation (max-width: 900px) {
		.session-header {
			grid-template-columns: minmax(0, 1fr);
			gap: var(--md-sys-space-sm);
			padding-block: var(--md-sys-space-sm);
		}
		.session-header__actions {
			flex-wrap: wrap;
			justify-content: space-between;
		}
		.session-header__toolbar,
		.session-header__lifecycle {
			flex-wrap: wrap;
		}
	}
	@container conversation (max-width: 455px) {
		.session-header__title-row {
			gap: var(--md-sys-space-sm);
		}
		.session-header__actions {
			gap: var(--md-sys-space-xs);
		}
		.session-header__toolbar {
			flex: 1 1 auto;
		}
		.session-header__lifecycle {
			flex: 1 1 100%;
		}
		.session-header__lifecycle :global(.md-btn) {
			flex: 1 1 5.5rem;
		}
	}
	@media screen and (max-height: 560px) {
		.session-header {
			gap: var(--md-sys-space-xs);
			padding-block: var(--md-sys-space-xs);
		}
	}
</style>
