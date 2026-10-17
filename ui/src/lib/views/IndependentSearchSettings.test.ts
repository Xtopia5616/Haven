import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { ModelDraft, ProviderDraft } from '$lib/settingsModelTypes.ts';
import IndependentSearchSettings from './IndependentSearchSettings.svelte';

function drafts(providerId: string) {
	const provider: ProviderDraft = {
		name: 'selected',
		provider: providerId,
		media_capabilities: { stt: false, tts: false, image_gen: false },
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
			country: null,
			max_tokens: null,
			max_tokens_per_page: null,
			search_language_filter: [],
			search_domain_filter: [],
			last_updated_after_filter: null,
			last_updated_before_filter: null,
			search_after_date_filter: null,
			search_before_date_filter: null,
		});
		expect(screen.getByLabelText('最多结果数')).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Perplexity 搜索类型' })).toBeTruthy();
	});

	it('shows the Alibaba direct search endpoint and separate key controls for DashScope', async () => {
		const { model, provider } = drafts('dashscope');
		const { rerender } = render(IndependentSearchSettings, { model, provider });

		await fireEvent.click(screen.getByRole('checkbox', { name: '启用' }));
		await rerender({ model: { ...model }, provider });

		expect(model.independent_search?.provider).toBe('aliyun_ai_search');
		expect(screen.getByLabelText('AI Search 接入地址')).toBeTruthy();
		expect(screen.getByLabelText('AI Search API Key')).toBeTruthy();
	});

	it('hides independent search controls for providers without a standalone product', () => {
		const { model, provider } = drafts('openai');
		render(IndependentSearchSettings, { model, provider });

		expect(screen.queryByText('独立搜索 API')).toBeNull();
	});
});
