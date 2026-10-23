import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SettingsHooks from './SettingsHooks.svelte';
import { emptyHooks } from '$lib/lifecycleHooksEditor.ts';
import type { TauriCommandResponse } from '$lib/contracts/generatedCommands.ts';

const { getSettings, saveHooks, reportError } = vi.hoisted(() => ({
	getSettings: vi.fn(),
	saveHooks: vi.fn(),
	reportError: vi.fn(),
}));
vi.mock('$lib/settingsCommands.ts', () => ({
	getLifecycleHooksSettings: getSettings,
	saveLifecycleHooks: saveHooks,
	trustHookWorkspace: vi.fn(),
	untrustHookWorkspace: vi.fn(),
}));
vi.mock('$lib/errorHandling.ts', () => ({ reportError }));
vi.mock('$lib/notificationStore.ts', () => ({ addNotification: vi.fn() }));

function fixture(): TauriCommandResponse<'get_lifecycle_hooks_settings'> {
	return {
		hooks: {
			...emptyHooks(),
			pre_tool_use: [
				{ enabled: true, command: 'hook.exe', args: ['original'], matcher: 'shell' },
			],
		},
		trusted_hook_workspaces: [],
		resource_policy: {
			files: { write_mode: 'unrestricted', write_roots: [], path_roots: [] },
			network: { access: 'open', allowed_domains: [] },
		},
		config_version: 1,
		diagnostics: [],
	};
}

describe('SettingsHooks', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		getSettings.mockResolvedValue(fixture());
		saveHooks.mockResolvedValue(undefined);
	});

	it('keeps the editor and unsaved input after a failed save and allows retry', async () => {
		saveHooks.mockRejectedValueOnce(new Error('write rejected'));
		render(SettingsHooks, { active: false, onDraftState: vi.fn() });
		const executable = await screen.findByLabelText('可执行文件');
		await fireEvent.input(executable, { target: { value: 'changed.exe' } });
		await fireEvent.click(screen.getByRole('button', { name: '保存 Hooks' }));
		await screen.findByRole('alert');
		expect((screen.getByLabelText('可执行文件') as HTMLInputElement).value).toBe('changed.exe');
		await fireEvent.click(screen.getByRole('button', { name: '保存 Hooks' }));
		await waitFor(() => expect(saveHooks).toHaveBeenCalledTimes(2));
		await waitFor(() =>
			expect(
				(screen.getByRole('button', { name: '保存 Hooks' }) as HTMLButtonElement).disabled,
			).toBe(true),
		);
		expect(saveHooks.mock.calls[1][0].pre_tool_use[0].command).toBe('changed.exe');
	});

	it('disables without deleting, saves a plain snapshot, and restores discarded state', async () => {
		render(SettingsHooks, { active: false, onDraftState: vi.fn() });
		const toggle = await screen.findByLabelText('启用');
		await fireEvent.click(toggle);
		await fireEvent.click(screen.getByRole('button', { name: '保存 Hooks' }));
		await waitFor(() => expect(saveHooks).toHaveBeenCalledOnce());
		await waitFor(() =>
			expect(
				(screen.getByRole('button', { name: '保存 Hooks' }) as HTMLButtonElement).disabled,
			).toBe(true),
		);
		expect(saveHooks.mock.calls[0][0].pre_tool_use[0]).toEqual({
			enabled: false,
			command: 'hook.exe',
			args: ['original'],
			matcher: 'shell',
		});
		await fireEvent.input(screen.getByLabelText('可执行文件'), {
			target: { value: 'other.exe' },
		});
		expect(saveHooks.mock.calls[0][0].pre_tool_use[0].command).toBe('hook.exe');
		await fireEvent.click(screen.getByRole('button', { name: '放弃改动' }));
		expect((screen.getByLabelText('可执行文件') as HTMLInputElement).value).toBe('hook.exe');
		expect((screen.getByLabelText('启用') as HTMLInputElement).checked).toBe(false);
	});

	it('adds a runtime template to the selected event and warns about non-tool matchers', async () => {
		render(SettingsHooks, { active: false, onDraftState: vi.fn() });
		await screen.findByLabelText('可执行文件');
		await fireEvent.change(screen.getByLabelText('事件筛选'), { target: { value: 'run_end' } });
		await fireEvent.change(screen.getByLabelText('新增命令模板'), {
			target: { value: 'node' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '添加 Hook' }));
		expect((screen.getByLabelText('可执行文件') as HTMLInputElement).value).toBe('node.exe');
		expect((screen.getByLabelText('参数（每行一个）') as HTMLTextAreaElement).value).toBe(
			'scripts/hook.mjs',
		);
		await fireEvent.input(screen.getByLabelText('工具 matcher'), {
			target: { value: 'shell' },
		});
		expect(screen.getByText(/此事件没有工具名/)).toBeTruthy();
		expect(saveHooks).not.toHaveBeenCalled();
	});

	it('keeps hidden validation errors blocking save, and does not execute matcher previews', async () => {
		render(SettingsHooks, { active: false, onDraftState: vi.fn() });
		await screen.findByLabelText('可执行文件');
		await fireEvent.input(screen.getByLabelText('测试工具名（仅预览）'), {
			target: { value: 'files.read' },
		});
		expect(screen.getByText(/不匹配此工具/)).toBeTruthy();
		await fireEvent.input(screen.getByLabelText('可执行文件'), { target: { value: '' } });
		await fireEvent.change(screen.getByLabelText('事件筛选'), { target: { value: 'run_end' } });
		expect(
			(screen.getByRole('button', { name: '保存 Hooks' }) as HTMLButtonElement).disabled,
		).toBe(true);
		expect(screen.getByText(/包含筛选隐藏的事件/)).toBeTruthy();
		expect(saveHooks).not.toHaveBeenCalled();
	});

	it('imports into the draft only and leaves the draft intact on invalid import', async () => {
		render(SettingsHooks, { active: false, onDraftState: vi.fn() });
		await screen.findByLabelText('可执行文件');
		await fireEvent.click(screen.getByText('导入 / 导出用户级配置'));
		const input = screen.getByLabelText('Hook JSON 配置');
		await fireEvent.input(input, { target: { value: '{"bad":[]}' } });
		await fireEvent.click(screen.getByRole('button', { name: '载入为草稿' }));
		expect((screen.getByLabelText('可执行文件') as HTMLInputElement).value).toBe('hook.exe');
		await fireEvent.input(input, {
			target: { value: '{"run_end":[{"command":"node.exe","enabled":false}]}' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '载入为草稿' }));
		expect((screen.getByLabelText('可执行文件') as HTMLInputElement).value).toBe('node.exe');
		expect((screen.getByLabelText('启用') as HTMLInputElement).checked).toBe(false);
		expect(saveHooks).not.toHaveBeenCalled();
	});

	it('distinguishes veto failure from notification failure in diagnostics', async () => {
		const settings = fixture();
		settings.diagnostics = ['pre_tool_use', 'run_end'].map((event) => ({
			event,
			event_runs: 1,
			command_runs: 0,
			failures: 1,
			denials: 0,
			last_outcome: 'failed_open',
			last_failure_code: 'security_policy_denied',
		})) as typeof settings.diagnostics;
		getSettings.mockResolvedValue(settings);
		render(SettingsHooks, { active: false, onDraftState: vi.fn() });
		await screen.findByLabelText('可执行文件');
		expect(screen.getByText('失败，已阻止工具')).toBeTruthy();
		expect(screen.getByText('失败，主操作继续')).toBeTruthy();
		const row = screen.getByText('失败，已阻止工具').parentElement!;
		expect(within(row).getByText(/检查当前沙箱和网络策略/)).toBeTruthy();
	});
});
