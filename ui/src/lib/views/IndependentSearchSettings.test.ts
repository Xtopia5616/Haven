import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { ModelDraft, ProviderDraft } from '$lib/settingsModelTypes.ts';
import IndependentSearchSettings from './IndependentSearchSettings.svelte';

function drafts(providerId: string) {
	const provider: ProviderDraft = {
		name: 'selected',
		provider: providerId,
		base_url: 'https://api.example.test',
	};
	const model: ModelDraft = {
		id: 'model-1',
		providerName: provider.name,
		model: 'model-name',
		capabilities: ['chat'],
	};
	return { model, provider };
}

describe('IndependentSearchSettings', () => {
	it('shows search options only for providers with standalone search APIs', () => {
		const { model, provider } = drafts('deepseek');
		render(IndependentSearchSettings, { model, provider });

		expect(screen.getByText('独立搜索 API')).toBeTruthy();
		expect(screen.getByRole('checkbox', { name: '启用' })).toBeTruthy();
		expect(screen.queryByText('返回结果数')).toBeNull();
		expect(screen.queryByText('Premium 搜索会增加已验证新闻来源。')).toBeNull();
	});

	it('turns the selected provider product into saved model configuration', async () => {
		const { model, provider } = drafts('perplexity');
		const { rerender } = render(IndependentSearchSettings, { model, provider });

		await fireEvent.click(screen.getByRole('checkbox', { name: '启用' }));
		await rerender({ model: { ...model }, provider });

		expect(model.independent_search).toEqual({
			provider: 'perplexity_search',
			max_results: 10,
			search_type: 'web',
			recency_filter: null,
		});
		expect(screen.getByLabelText('最多结果数')).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Perplexity 搜索类型' })).toBeTruthy();
	});

	it('hides independent search controls for providers without a standalone product', () => {
		const { model, provider } = drafts('openai');
		render(IndependentSearchSettings, { model, provider });

		expect(screen.queryByText('独立搜索 API')).toBeNull();
	});
});
