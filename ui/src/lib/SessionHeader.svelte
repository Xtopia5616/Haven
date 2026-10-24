<script lang="ts">
	import type { Snippet } from 'svelte';
	import MaterialButton from './MaterialButton.svelte';
	import MaterialCollapsible from './MaterialCollapsible.svelte';
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
	<div class="session-header__main">
		<h1 {title}>{title}</h1>
		<div class="session-header__lifecycle">
			<MaterialButton
				width="compact"
				variant="tonal"
				className="session-header__new"
				label="新建会话"
				onclick={() => onNew?.()}
			/>
			{#if hasSession && canEndSession}
				<MaterialButton
					width="compact"
					variant="text"
					className="session-header__end"
					label="完成会话"
					onclick={() => onEnd?.()}
				/>
			{/if}
		</div>
	</div>
	<div class="session-header__secondary">
		<div class="session-header__toolbar">{@render children?.()}</div>
		<div class="session-header__context">
			<MaterialCollapsible lazy>
				{#snippet header()}
					<span class="session-header__context-label">会话设置</span>
					<span
						class="session-header__context-summary"
						title={executionWorkspaceRoot || '新会话默认目录'}
					>
						{executionModeLabels[executionMode]} · {executionWorkspaceRoot
							?.split(/[\\/]/)
							.filter(Boolean)
							.at(-1) || '默认目录'}
					</span>
				{/snippet}
				{#snippet children()}
					<div class="session-header__settings" role="group" aria-label="会话执行上下文">
						<div class="session-header__workspace-controls">
							<div class="session-header__workspace-picker">
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
									disabled={executionWorkspaceSwitching ||
										!selectedWorkspaceIsSaved}
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
									disabled={executionWorkspaceSwitching ||
										!onAddExecutionWorkspace}
									title="选择当前会话的工作目录，并保存快捷项"
									onclick={() => onAddExecutionWorkspace?.()}
								/>
							</div>
							<MaterialSelect
								id="session-execution-mode"
								ariaLabel="会话执行方式"
								width="standard"
								value={executionMode}
								options={executionModeOptions}
								title={`执行方式：${executionModeLabels[executionMode]}`}
								disabled={executionModeSwitching || !onSelectExecutionMode}
								onChange={(mode) => {
									if (
										mode === 'analyze' ||
										mode === 'collaborative' ||
										mode === 'unattended'
									)
										onSelectExecutionMode?.(mode);
								}}
							/>
						</div>
						{#if hasSession}
							<MaterialButton
								width="compact"
								variant="danger-text"
								label="删除会话"
								onclick={() => onDelete?.()}
							/>
						{/if}
					</div>
				{/snippet}
			</MaterialCollapsible>
		</div>
	</div>
</header>

<style>
	.session-header {
		position: relative;
		z-index: 6;
		flex: none;
		min-width: 0;
		padding: var(--md-sys-space-md) var(--chat-inline-gutter, var(--md-sys-space-lg));
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface);
	}
	.session-header__main,
	.session-header__secondary {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.session-header h1 {
		flex: 1;
		min-width: 0;
		margin: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--chat-title-size, var(--md-sys-typescale-title-large-size));
		font-weight: 600;
		line-height: var(--md-sys-typescale-title-large-line-height);
	}
	.session-header__lifecycle,
	.session-header__toolbar {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		flex-wrap: wrap;
	}
	.session-header__lifecycle {
		flex: none;
	}
	.session-header__lifecycle :global(.md-btn) {
		width: auto;
		min-width: 0;
	}
	.session-header__secondary {
		margin-top: var(--md-sys-space-xs);
		align-items: flex-start;
	}
	.session-header__context {
		flex: 1;
		min-width: 0;
	}
	.session-header__context :global(.md-collapsible-header) {
		min-height: var(--md-comp-toolbar-height);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.session-header__context :global(.md-collapsible-header-content) {
		flex-wrap: wrap;
	}
	.session-header__context :global(.md-collapsible-body-content) {
		overflow: visible;
	}
	.session-header__context-label {
		flex: none;
		font-weight: 600;
	}
	.session-header__context-summary {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		margin-left: var(--md-sys-space-xs);
	}
	.session-header__settings {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		flex-wrap: wrap;
		padding-block: var(--md-sys-space-sm);
	}
	.session-header__workspace-picker {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto auto;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		flex: 1 1 16rem;
	}
	.session-header__workspace-controls {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		flex: 1 1 100%;
	}
	.session-header__workspace-controls :global(.md-select-container) {
		min-width: 0;
		max-width: 100%;
		flex: 1 1 10rem;
	}
	@container conversation (max-width: 455px) {
		.session-header {
			display: grid;
			grid-template-columns: minmax(0, 1fr) auto;
			gap: var(--md-sys-space-xs);
			padding-block: var(--md-sys-space-sm);
		}
		.session-header__main,
		.session-header__secondary {
			display: contents;
		}
		.session-header h1 {
			grid-column: 1 / -1;
		}
		.session-header__toolbar {
			grid-column: 1;
			grid-row: 2;
		}
		.session-header__lifecycle {
			grid-column: 2;
			grid-row: 2;
		}
		.session-header__lifecycle :global(.md-btn) {
			padding-inline: var(--md-sys-space-sm);
		}
		.session-header__context {
			grid-column: 1 / -1;
		}
	}

	@media screen and (max-height: 560px) {
		.session-header {
			padding-block: var(--md-sys-space-xs);
		}
		.session-header__secondary {
			margin-top: 0;
		}
	}
</style>
