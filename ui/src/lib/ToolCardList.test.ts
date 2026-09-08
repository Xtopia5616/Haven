import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/svelte';
import ToolCardList from './ToolCardList.svelte';

describe('ToolCardList', () => {
	it('provides the shared structured-result list surface', () => {
		render(ToolCardList);
		expect(document.querySelector('.tool-card-list')).not.toBeNull();
	});
});
