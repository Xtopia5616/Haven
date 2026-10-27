import type { ExecutionMode } from './contracts/generatedCommands.ts';

interface Dependencies {
	getSelection: () => { sessionId: string | null; version: number };
	setWorkspace: (sessionId: string, root: string | null) => Promise<string | null>;
	setMode: (sessionId: string, mode: ExecutionMode) => Promise<ExecutionMode>;
	setDefaultWorkspace: (root: string | null) => Promise<unknown>;
	setDefaultMode: (mode: ExecutionMode) => Promise<unknown>;
	pickWorkspace: () => Promise<string | null>;
	applyWorkspace: (root: string | null) => void;
	applyMode: (mode: ExecutionMode) => void;
	applyDefaultWorkspace: (root: string | null) => void;
	applyDefaultMode: (mode: ExecutionMode) => void;
	rememberWorkspace: (root: string) => void;
	invalidateWorkspaceRead: () => void;
	invalidateModeRead: () => void;
	notify: (message: string) => void;
}

/** Coordinates execution preference writes with the chat selection that requested them. */
export function createChatExecutionController(deps: Dependencies) {
	function isCurrent(selection: ReturnType<Dependencies['getSelection']>) {
		const current = deps.getSelection();
		return current.sessionId === selection.sessionId && current.version === selection.version;
	}
	async function saveWorkspace(
		root: string | null,
		selection: ReturnType<Dependencies['getSelection']>,
	) {
		deps.invalidateWorkspaceRead();
		if (selection.sessionId) {
			const saved = await deps.setWorkspace(selection.sessionId, root);
			if (isCurrent(selection)) {
				deps.applyWorkspace(saved);
				deps.notify('当前会话工作目录已更新');
			}
		} else {
			await deps.setDefaultWorkspace(root);
			deps.applyDefaultWorkspace(root);
			if (isCurrent(selection)) {
				deps.applyWorkspace(root);
				deps.notify('新会话默认工作目录已更新');
			}
		}
		if (root) deps.rememberWorkspace(root);
	}
	return {
		workspace: (root: string | null) => saveWorkspace(root, deps.getSelection()),
		async pickWorkspace() {
			const selection = deps.getSelection();
			const root = await deps.pickWorkspace();
			if (root !== null && isCurrent(selection)) await saveWorkspace(root, selection);
		},
		async mode(mode: ExecutionMode) {
			const selection = deps.getSelection();
			deps.invalidateModeRead();
			if (selection.sessionId) {
				const saved = await deps.setMode(selection.sessionId, mode);
				if (isCurrent(selection)) deps.applyMode(saved);
			} else {
				await deps.setDefaultMode(mode);
				deps.applyDefaultMode(mode);
				if (isCurrent(selection)) deps.applyMode(mode);
			}
			if (isCurrent(selection)) deps.notify('会话执行方式已更新');
		},
	};
}
