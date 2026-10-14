import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import McpServerCard from './McpServerCard.svelte';
import type { McpServerSnapshotView, McpToolInfoView } from './contracts/mcp.ts';

function server(tools: McpToolInfoView[]): McpServerSnapshotView {
	return {
		name: 'docs-server',
		transport: 'stdio',
		command: 'server.exe',
		args: [],
		env: [],
		cwd: null,
		url: '',
		enabled: true,
		exposure: { default: 'deferred', tools: {} },
		status: 'Connected',
		tools,
		lastError: null,
		diagnostic: null,
		lastSeenAt: null,
	};
}

function tool(name: string): McpToolInfoView {
	return { name, description: `${name} description`, inputSchema: { type: 'object', name } };
}

describe('McpServerCard', () => {
	it('keeps schema disclosure state with the tool when the server list is reordered', async () => {
		const { rerender } = render(McpServerCard, { server: server([tool('alpha'), tool('beta')]) });
		const cardHeader = screen.getByText('docs-server').closest('[role="button"]');
		expect(cardHeader).toBeTruthy();
		await fireEvent.click(cardHeader!);

		const alphaRow = screen.getByText('alpha').closest('.tool-item');
		const alphaSchemaButton = alphaRow?.querySelector('.md-collapsible-header');
		expect(alphaSchemaButton).toBeTruthy();
		await fireEvent.click(alphaSchemaButton!);
		expect(alphaSchemaButton?.getAttribute('aria-expanded')).toBe('true');

		rerender({ server: server([tool('beta'), tool('alpha')]) });

		const reorderedAlphaRow = screen.getByText('alpha').closest('.tool-item');
		const reorderedBetaRow = screen.getByText('beta').closest('.tool-item');
		expect(reorderedAlphaRow?.querySelector('.md-collapsible-header')?.getAttribute('aria-expanded')).toBe(
			'true',
		);
		expect(reorderedBetaRow?.querySelector('.md-collapsible-header')?.getAttribute('aria-expanded')).toBe(
			'false',
		);
	});
});
