import { get, writable } from 'svelte/store';

const STORAGE_KEY = 'haven.execution-workspaces';
export const MAX_SAVED_EXECUTION_WORKSPACES = 20;

function normalizePath(path: string): string | null {
	const normalized = path.trim().replace(/[\\/]/g, '\\');
	if (!normalized) return null;
	if (/^[a-zA-Z]:\\+$/.test(normalized)) return `${normalized.slice(0, 2)}\\`;
	return normalized.replace(/\\+$/, '');
}

function pathKey(path: string): string | null {
	return normalizePath(path)?.toLocaleLowerCase('en-US') ?? null;
}

export function sameExecutionWorkspacePath(left: string, right: string): boolean {
	const leftKey = pathKey(left);
	return leftKey !== null && leftKey === pathKey(right);
}

function uniquePaths(paths: unknown[]): string[] {
	const seen = new Set<string>();
	const result: string[] = [];
	for (const candidate of paths) {
		if (typeof candidate !== 'string') continue;
		const normalized = normalizePath(candidate);
		const key = normalized ? pathKey(normalized) : null;
		if (!normalized || !key || seen.has(key)) continue;
		seen.add(key);
		result.push(normalized);
		if (result.length >= MAX_SAVED_EXECUTION_WORKSPACES) break;
	}
	return result;
}

function readStorage(): string[] {
	if (typeof window === 'undefined') return [];
	try {
		const serialized = window.localStorage.getItem(STORAGE_KEY);
		if (!serialized) return [];
		const parsed: unknown = JSON.parse(serialized);
		return Array.isArray(parsed) ? uniquePaths(parsed) : [];
	} catch {
		return [];
	}
}

function writeStorage(paths: string[]): void {
	if (typeof window === 'undefined') return;
	try {
		window.localStorage.setItem(STORAGE_KEY, JSON.stringify(paths));
	} catch {
		// Keep the current process usable if browser storage is unavailable.
	}
}

function createExecutionWorkspaceStore() {
	// Hydrate onMount so SSR and the first client render share the same empty
	// value; persisted paths appear as soon as the UI is mounted.
	const state = writable<string[]>([]);

	function publish(paths: string[]) {
		const boundedPaths = uniquePaths(paths);
		writeStorage(boundedPaths);
		state.set(boundedPaths);
	}

	return {
		subscribe: state.subscribe,
		refresh() {
			state.set(readStorage());
		},
		add(path: string): void {
			const normalized = normalizePath(path);
			const key = normalized ? pathKey(normalized) : null;
			if (!normalized || !key) return;
			const current = get(state).filter((savedPath) => pathKey(savedPath) !== key);
			publish([normalized, ...current]);
		},
		remove(path: string): void {
			const key = pathKey(path);
			if (!key) return;
			publish(get(state).filter((savedPath) => pathKey(savedPath) !== key));
		},
	};
}

export const executionWorkspaceStore = createExecutionWorkspaceStore();
