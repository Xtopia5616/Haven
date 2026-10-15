<script lang="ts">
	import MaterialButton from './MaterialButton.svelte';
	import {
		discardSandboxOutputReview,
		getSandboxOutputReview,
		pickSandboxOutputDestination,
		promoteSandboxOutput,
	} from './toolsCommands.ts';
	import { formatError, isAuthorizationConfirmationPending } from './formatError.ts';
	import type {
		SandboxOutputReviewChangeDto,
		SandboxOutputReviewManifestDto,
	} from './contracts/generatedCommands.ts';

	interface Props {
		sessionId: string;
		sourceStepId: string;
	}

	let { sessionId, sourceStepId }: Props = $props();
	let manifest = $state<SandboxOutputReviewManifestDto | null>(null);
	let loading = $state(false);
	let busy = $state(false);
	let pendingChangeIndex = $state<number | null>(null);
	let statusMessage = $state('');
	let errorMessage = $state('');

	$effect(() => {
		const currentSessionId = sessionId;
		const currentStepId = sourceStepId;
		let stale = false;
		manifest = null;
		statusMessage = '';
		errorMessage = '';
		pendingChangeIndex = null;
		if (!currentSessionId || !currentStepId) return;
		void refresh(currentSessionId, currentStepId, () => stale);
		return () => {
			stale = true;
		};
	});

	async function refresh(
		currentSessionId = sessionId,
		currentStepId = sourceStepId,
		isStale: () => boolean = () => false,
	) {
		loading = true;
		errorMessage = '';
		try {
			const next = await getSandboxOutputReview({
				sessionId: currentSessionId,
				sourceStepId: currentStepId,
			});
			if (isStale()) return;
			manifest = next;
			if (pendingChangeIndex !== null) {
				const current = next?.changes.find((change) => change.changeIndex === pendingChangeIndex);
				statusMessage = current?.promoted
					? '文件已创建到你选择的位置。'
					: '此文件尚未保存；你可以重新选择位置并请求一次授权。';
				pendingChangeIndex = null;
			}
		} catch (error) {
			if (!isStale()) errorMessage = formatError(error);
		} finally {
			if (!isStale()) loading = false;
		}
	}

	function fileSize(value: number | null): string {
		if (value == null) return '未知大小';
		if (value < 1024) return `${value} B`;
		if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
		return `${(value / (1024 * 1024)).toFixed(1)} MB`;
	}

	function changeLabel(change: SandboxOutputReviewChangeDto['change']): string {
		return {
			created: '新增',
			modified: '已修改',
			deleted: '已删除',
			type_changed: '类型改变',
		}[change];
	}

	async function promote(change: SandboxOutputReviewChangeDto) {
		if (busy || change.promoted || change.change !== 'created') return;
		const currentSessionId = sessionId;
		const currentStepId = sourceStepId;
		busy = true;
		errorMessage = '';
		statusMessage = '';
		try {
			const destination = await pickSandboxOutputDestination(change.relativePath);
			if (!destination) return;
			await promoteSandboxOutput({
				sessionId: currentSessionId,
				sourceStepId: currentStepId,
				changeIndex: change.changeIndex,
				destination,
			});
		} catch (error) {
			if (isAuthorizationConfirmationPending(error)) {
				pendingChangeIndex = change.changeIndex;
				statusMessage = '请在权限窗口中处理一次性授权；处理后刷新此审阅状态。';
			} else {
				errorMessage = formatError(error);
			}
		} finally {
			busy = false;
		}
	}

	async function discard() {
		if (busy) return;
		const currentSessionId = sessionId;
		const currentStepId = sourceStepId;
		busy = true;
		errorMessage = '';
		statusMessage = '';
		try {
			await discardSandboxOutputReview({
				sessionId: currentSessionId,
				sourceStepId: currentStepId,
			});
			manifest = null;
			statusMessage = '临时审阅数据已丢弃。';
		} catch (error) {
			errorMessage = formatError(error);
		} finally {
			busy = false;
		}
	}
</script>

{#if loading || manifest || errorMessage || statusMessage}
	<section class="sandbox-review" aria-label="Sandbox 输出审阅" data-testid="sandbox-output-review">
		{#if manifest}
			<header>
				<div>
					<strong>审阅隔离运行的文件变化</strong>
					<p>只显示变化摘要。文件内容仍留在隔离区，单个新增文件需逐项授权后保存。</p>
				</div>
				<MaterialButton
					variant="text"
					label="刷新状态"
					disabled={loading || busy}
					onclick={() => refresh()}
				/>
			</header>
			<ul>
				{#each manifest.changes as change (change.changeIndex)}
					<li data-change={change.change} data-promoted={change.promoted}>
						<div class="file-info">
							<code>{change.relativePath}</code>
							<span>{changeLabel(change.change)}</span>
							<small>
								{change.beforeBytes == null
									? ''
									: `原 ${fileSize(change.beforeBytes)} · `}{change.afterBytes == null
								? '没有新文件内容'
								: `后 ${fileSize(change.afterBytes)}`}
							</small>
						</div>
						{#if change.promoted}
							<span class="saved-state">已保存</span>
						{:else if change.change === 'created'}
							<MaterialButton
								variant="filled"
								label="选择位置并保存"
								disabled={busy || pendingChangeIndex !== null}
								onclick={() => promote(change)}
							/>
						{:else}
							<span class="review-only">仅供查看</span>
						{/if}
					</li>
				{/each}
			</ul>
			<footer>
				<span>审阅会在 15 分钟后自动清理</span>
				<MaterialButton
					variant="danger"
					label="丢弃全部审阅数据"
					disabled={busy || pendingChangeIndex !== null}
					onclick={discard}
				/>
			</footer>
		{:else if loading}
			<p class="message">正在检查此 Shell 步骤是否有待审阅输出…</p>
		{/if}
		{#if statusMessage}<p class="message" role="status">{statusMessage}</p>{/if}
		{#if errorMessage}<p class="message error" role="alert">{errorMessage}</p>{/if}
	</section>
{/if}

<style>
	.sandbox-review {
		margin-top: 10px;
		padding: 12px;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-corner-medium);
		background: var(--md-sys-color-surface-container-low);
	}

	header,
	footer,
	li {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}

	header strong {
		font-size: 0.9rem;
	}

	header p,
	.message {
		margin: 4px 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 0.78rem;
	}

	ul {
		margin: 10px 0;
		padding: 0;
		list-style: none;
	}

	li {
		padding: 9px 0;
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}

	.file-info {
		min-width: 0;
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 3px 10px;
	}

	.file-info code {
		grid-column: 1 / -1;
		overflow-wrap: anywhere;
		color: var(--md-sys-color-on-surface);
	}

	.file-info small,
	.review-only,
	.saved-state {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 0.75rem;
	}

	.saved-state {
		color: var(--md-sys-color-primary);
	}

	footer {
		padding-top: 8px;
		border-top: 1px solid var(--md-sys-color-outline-variant);
		font-size: 0.75rem;
		color: var(--md-sys-color-on-surface-variant);
	}

	.message.error {
		color: var(--md-sys-color-error);
	}

	@media (max-width: 520px) {
		li {
			align-items: flex-start;
			flex-direction: column;
		}
	}
</style>
