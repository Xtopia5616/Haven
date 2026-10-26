import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import ConfirmationDialog from './ConfirmationDialog.svelte';

const deadlineProps = (durationMs = 60_000) => ({
	createdAt: new Date().toISOString(),
	deadlineAt: Date.now() + durationMs,
});

describe('ConfirmationDialog', () => {
	it('shows concrete invocation details and folds credential-like values by default', async () => {
		const longArgument = `--input ${'x'.repeat(180)}`;
		const { container } = render(ConfirmationDialog as any, {
			...deadlineProps(),
			requestId: 'conf-4c7a29d6b2c347b0a83e901ef7654321',
			toolName: 'shell',
			riskLevel: 'high',
			summary: '将执行一条受保护的本机命令；请检查下方调用参数。',
			toolInput: {
				command: 'cargo test --workspace',
				cwd: 'D:\\Workspace\\Haven',
				payload: longArgument,
				options: { transport: { retry_limit: 3 } },
				api_key: 'example-value-123',
			},
			onConfirm: vi.fn(),
		});

		expect(screen.getByText('"cargo test --workspace"')).toBeTruthy();
		expect(screen.getByText('"D:\\\\Workspace\\\\Haven"')).toBeTruthy();
		expect(screen.getByText(JSON.stringify(longArgument))).toBeTruthy();
		expect(screen.queryByText(/example-value-123/)).toBeNull();
		expect(container.textContent).not.toContain('retry_limit');
		const bodyChildren = container.querySelector('.dialog-body')?.children;
		expect(bodyChildren?.[0].classList.contains('summary')).toBe(true);
		expect(bodyChildren?.[1].classList.contains('operation-card')).toBe(true);
		const sensitiveDetails =
			container.querySelector<HTMLDetailsElement>('details.jv-sensitive');
		expect(sensitiveDetails?.open).toBe(false);

		await fireEvent.click(sensitiveDetails!.querySelector('summary')!);
		expect(sensitiveDetails?.open).toBe(true);
		expect(screen.getByText(/example-value-123/)).toBeTruthy();
	});

	it('keeps one-time approval primary and moves broader grants into more options', async () => {
		const onConfirm = vi.fn();
		const { container } = render(ConfirmationDialog as any, {
			...deadlineProps(),
			requestId: 'conf-b7a65e97699c7c548225828bfc5e17f9',
			toolName: 'files.write',
			sessionId: 'ses-f598facda111677e0871bdc09d391332',
			sessionTitle: '测试会话',
			riskLevel: 'medium',
			summary: '写入文件',
			permissionKey: 'files.write',
			resources: [{ kind: 'local_path', path: 'C:\\notes.txt', access: 'write' }],
			onConfirm,
		});

		const onceButton = screen.getByRole('button', { name: '本次允许' });
		const moreAllowButton = screen.getByRole('button', { name: '更多允许' });
		expect(onceButton.classList.contains('md-btn--filled')).toBe(true);
		expect(moreAllowButton.classList.contains('md-btn--text')).toBe(true);
		expect(moreAllowButton.getAttribute('aria-haspopup')).toBe('menu');
		expect(
			screen.getByText('本会话授权不会按时间自动到期；撤销、清除授权或删除会话后失效'),
		).toBeTruthy();
		expect(
			screen.getByRole('button', { name: '更多拒绝选项' }).getAttribute('data-variant'),
		).toBe('danger');

		const allowButtonLabels = Array.from(container.querySelectorAll('.allow-group button')).map(
			(button) => button.textContent?.trim() || button.getAttribute('aria-label'),
		);
		expect(allowButtonLabels).toEqual(['本次允许', '本会话允许', '更多允许']);
		expect(
			container
				.querySelector('.actions')
				?.firstElementChild?.classList.contains('allow-group'),
		).toBe(true);
		expect(
			container.querySelector('.actions')?.lastElementChild?.classList.contains('deny-split'),
		).toBe(true);

		await fireEvent.click(moreAllowButton);
		expect(screen.getByRole('menuitem', { name: '永久允许此操作' })).toBeTruthy();
		expect(screen.getByText(/功能组和工具规则会匹配对应权限键及其所有下级操作/)).toBeTruthy();
		expect(moreAllowButton.getAttribute('aria-expanded')).toBe('true');

		await fireEvent.click(screen.getByRole('button', { name: '更多拒绝选项' }));
		expect(screen.getByText(/拒绝会阻止对应权限键及其所有下级操作/)).toBeTruthy();
		expect(
			Array.from(container.querySelectorAll('.deny-menu .menu-item')).every((item) =>
				item.classList.contains('danger'),
			),
		).toBe(true);

		await fireEvent.click(onceButton);
		expect(onConfirm).toHaveBeenCalledWith({
			requestId: 'conf-b7a65e97699c7c548225828bfc5e17f9',
			approved: true,
			effect: 'allow',
			scope: 'once',
			target: 'operation',
		});
	});

	it('warns that a Shell file denial grants the entire parent directory for this retry', () => {
		render(ConfirmationDialog as any, {
			...deadlineProps(),
			requestId: 'conf-bd5d1f2e30fd45cb92561bb0ee89f12f',
			toolName: 'shell',
			summary: '首轮拒绝的文件将映射到父目录权限。',
			resources: [{ kind: 'local_path', path: 'C:\\Projects\\demo', access: 'write' }],
			resourceBoundaryExtension: true,
			onConfirm: vi.fn(),
		});

		expect(screen.getByRole('alert').textContent).toContain(
			'整个目录及其子目录和文件，不限于首轮被拒绝的单个文件',
		);
		expect(screen.getByText(/目录授权覆盖列出的目录及其子项/)).toBeTruthy();
		expect(screen.getByRole('button', { name: '临时扩展并允许本次' })).toBeTruthy();
	});

	it('explains a broad persistent grant and deny precedence before final confirmation', async () => {
		render(ConfirmationDialog as any, {
			...deadlineProps(),
			requestId: 'conf-44c0d5bc635784cc9b4504c8b21185dc',
			toolName: 'system.power.lock',
			sessionId: 'ses-f598facda111677e0871bdc09d391332',
			permissionKey: 'system.power.lock',
			resources: [{ kind: 'local_path', path: 'C:\\notes.txt', access: 'write' }],
			onConfirm: vi.fn(),
		});

		await fireEvent.click(screen.getByRole('button', { name: '更多允许' }));
		await fireEvent.click(screen.getByRole('menuitem', { name: /永久允许此功能组/ }));

		expect(screen.getByText(/即将永久允许此功能组（system.power）/)).toBeTruthy();
		expect(
			screen.getByText(
				/匹配“system.power”本身及所有以“system.power.”开头的下级操作或工具能力/,
			),
		).toBeTruthy();
		expect(screen.getByText(/明确资源授权优先于匹配的拒绝规则/)).toBeTruthy();
	});

	it('closes without deciding and can be shown again while the request stays pending', async () => {
		const onDismiss = vi.fn();
		const onConfirm = vi.fn();
		const deadline = deadlineProps();
		const { rerender } = render(ConfirmationDialog as any, {
			...deadline,
			open: true,
			requestId: 'conf-a01e8dd02b9ac9753f4d8ceeb93b4af0',
			toolName: 'files.write',
			onDismiss,
			onConfirm,
		});

		await fireEvent.click(screen.getByRole('button', { name: '关闭权限确认' }));
		expect(onDismiss).toHaveBeenCalledWith('conf-a01e8dd02b9ac9753f4d8ceeb93b4af0');
		expect(onConfirm).not.toHaveBeenCalled();

		await rerender({
			...deadline,
			open: false,
			requestId: 'conf-a01e8dd02b9ac9753f4d8ceeb93b4af0',
			toolName: 'files.write',
			onDismiss,
			onConfirm,
		});
		await waitFor(() => expect(screen.queryByRole('button', { name: '本次允许' })).toBeNull());

		await rerender({
			...deadline,
			open: true,
			requestId: 'conf-a01e8dd02b9ac9753f4d8ceeb93b4af0',
			toolName: 'files.write',
			onDismiss,
			onConfirm,
		});
		await waitFor(() => expect(screen.getByRole('button', { name: '本次允许' })).toBeTruthy());
		expect(onConfirm).not.toHaveBeenCalled();
	});

	it('displays expiry without deciding when the dialog reaches its deadline', async () => {
		vi.useFakeTimers();
		try {
			const onConfirm = vi.fn();
			render(ConfirmationDialog as any, {
				...deadlineProps(1000),
				requestId: 'conf-55ddccbc8eb4825ca74dea3d65ce9ce5',
				toolName: 'files.write',
				sessionId: 'ses-f598facda111677e0871bdc09d391332',
				summary: '写入文件',
				permissionKey: 'files.write',
				onConfirm,
			});

			await vi.advanceTimersByTimeAsync(1500);
			expect(onConfirm).not.toHaveBeenCalled();
			expect(screen.getByText('0s')).toBeTruthy();
		} finally {
			vi.useRealTimers();
		}
	});

	it('does not offer session grants to confirmations without a persisted session', async () => {
		render(ConfirmationDialog as any, {
			...deadlineProps(),
			requestId: 'conf-16c894afc7e907aa73537a342ea11872',
			toolName: 'mcp__server__write',
			permissionKey: 'mcp.server.write',
			resources: [
				{
					kind: 'network_destination',
					scheme: 'https',
					host: 'api.example.test',
					port: null,
				},
			],
			onConfirm: vi.fn(),
		});

		expect(screen.queryByRole('button', { name: '本会话允许此操作' })).toBeNull();
		expect(screen.getByText('永久授权跨会话生效，关键操作仍可能继续要求确认')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '更多允许' }));
		expect(screen.queryByText('本会话')).toBeNull();
		expect(screen.getByRole('menuitem', { name: '永久允许此操作' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '更多拒绝选项' }));
		expect(screen.queryByRole('menuitem', { name: '本会话拒绝此操作' })).toBeNull();
	});

	it('keeps a rejected pre-execution confirmation submission retryable', async () => {
		const onConfirm = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
		render(ConfirmationDialog as any, {
			...deadlineProps(),
			requestId: 'conf-b1e0260f9f155eaca25bb42b04a91e5d',
			toolName: 'mcp__server__write',
			resources: [
				{
					kind: 'network_destination',
					scheme: 'https',
					host: 'api.example.test',
					port: null,
				},
			],
			onConfirm,
		});

		await fireEvent.click(screen.getByRole('button', { name: '本次允许' }));
		await fireEvent.click(screen.getByRole('button', { name: '本次允许' }));
		expect(onConfirm).toHaveBeenCalledTimes(2);
	});
});
