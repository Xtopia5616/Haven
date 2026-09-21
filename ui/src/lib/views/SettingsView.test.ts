import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SettingsView from './SettingsView.svelte';

const { invoke, listen } = vi.hoisted(() => ({
	invoke: vi.fn(),
	listen: vi.fn(),
}));

vi.mock('$lib/tauri.ts', () => ({ invoke, listen }));

describe('SettingsView diagnostics export', () => {
	beforeEach(() => {
		invoke.mockImplementation(async (command: string) => {
			switch (command) {
				case 'get_settings':
					return null;
				case 'get_api_key_status':
					return { models: {}, providers: {}, stt: false, ocr: false, ocr_secret: false };
				case 'is_autostart_enabled':
					return false;
				case 'get_performance_metrics':
					return { backend: { requests: 1 }, renderer: { frames: 2 } };
				default:
					return [];
			}
		});
		listen.mockResolvedValue(() => {});
	});

	it('creates and clicks a JSON download when the export button is pressed', async () => {
		const createObjectURL = vi.fn<(blob: Blob) => string>(() => 'blob:performance-metrics');
		const revokeObjectURL = vi.fn();
		Object.defineProperty(URL, 'createObjectURL', {
			configurable: true,
			value: createObjectURL,
		});
		Object.defineProperty(URL, 'revokeObjectURL', {
			configurable: true,
			value: revokeObjectURL,
		});
		const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});

		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));
		await fireEvent.click(screen.getByRole('button', { name: '导出性能指标' }));

		expect(invoke).toHaveBeenCalledWith('get_performance_metrics', undefined);
		expect(createObjectURL).toHaveBeenCalledOnce();
		const blob = createObjectURL.mock.calls[0][0] as Blob;
		expect(await blob.text()).toContain('"requests": 1');
		expect(click).toHaveBeenCalledOnce();
		const link = click.mock.instances[0] as HTMLAnchorElement;
		expect(link.download).toMatch(/^haven-performance-metrics-.*\.json$/);
		expect(revokeObjectURL).toHaveBeenCalledWith('blob:performance-metrics');

		click.mockRestore();
	});

	it('keeps permission management on its own settings tab', async () => {
		render(SettingsView);
		await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_api_key_status'));

		await fireEvent.click(screen.getByRole('tab', { name: /权限/ }));
		expect(screen.getByRole('heading', { name: '权限中心' })).toBeTruthy();
	});
});
