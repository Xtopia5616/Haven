import { beforeEach, describe, expect, it } from 'vitest';
import {
	executionWorkspaceStore,
	MAX_SAVED_EXECUTION_WORKSPACES,
} from './executionWorkspaceStore.ts';

const STORAGE_KEY = 'haven.execution-workspaces';

function currentWorkspaces(): string[] {
	let current: string[] = [];
	const unsubscribe = executionWorkspaceStore.subscribe((paths) => {
		current = paths;
	});
	unsubscribe();
	return current;
}

describe('executionWorkspaceStore', () => {
	beforeEach(() => {
		localStorage.clear();
		executionWorkspaceStore.refresh();
	});

	it('saves selected directories and moves an existing path to the front', () => {
		executionWorkspaceStore.add('C:\\Projects\\Haven');
		executionWorkspaceStore.add('D:/Projects/Tools/');
		executionWorkspaceStore.add('c:\\projects\\haven\\');

		expect(currentWorkspaces()).toEqual(['c:\\projects\\haven', 'D:\\Projects\\Tools']);
		expect(JSON.parse(localStorage.getItem(STORAGE_KEY) || '[]')).toEqual(currentWorkspaces());
	});

	it('loads valid saved paths and enforces the fixed list cap', () => {
		const paths = Array.from(
			{ length: MAX_SAVED_EXECUTION_WORKSPACES + 3 },
			(_, index) => `C:\\Projects\\${index}`,
		);
		localStorage.setItem(STORAGE_KEY, JSON.stringify(paths));
		executionWorkspaceStore.refresh();

		expect(currentWorkspaces()).toHaveLength(MAX_SAVED_EXECUTION_WORKSPACES);
		expect(currentWorkspaces()[0]).toBe('C:\\Projects\\0');
	});

	it('removes only the selected shortcut and leaves unrelated paths', () => {
		executionWorkspaceStore.add('C:\\Projects\\Haven');
		executionWorkspaceStore.add('D:\\Shared');
		executionWorkspaceStore.remove('c:/projects/haven/');

		expect(currentWorkspaces()).toEqual(['D:\\Shared']);
	});
});
