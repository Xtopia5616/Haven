import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import ToolNestedResult from './ToolNestedResult.svelte';

describe('ToolNestedResult', () => {
	it('renders the nested result payload', () => {
		render(ToolNestedResult, { data: { status: 'completed' } });
		expect(screen.getByTitle('"completed"')).toBeTruthy();
	});
});
