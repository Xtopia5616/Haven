import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import ToolMcpResult from './ToolMcpResult.svelte';

describe('ToolMcpResult', () => {
	it('renders the MCP result payload', () => {
		render(ToolMcpResult, { data: { result: 'done' } });
		expect(screen.getByTitle('"done"')).toBeTruthy();
	});
});
