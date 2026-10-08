import { describe, expect, it } from 'vitest';
import providerListSource from './ProviderList.svelte?raw';

describe('ProviderList responsive layout', () => {
	it('keeps the provider toolbar within its available content width', () => {
		expect(providerListSource).toMatch(/\.provider-toolbar\s*\{[^}]*flex-wrap:\s*wrap;/s);
		expect(providerListSource).toMatch(
			/\.provider-toolbar-actions\s*\{[^}]*flex-wrap:\s*wrap;[^}]*flex:\s*0 1 auto;[^}]*max-width:\s*100%;/s,
		);
		expect(providerListSource).toContain('@container settings-content (max-width: 700px)');
		expect(providerListSource).toContain('@container settings-content (max-width: 455px)');
	});
});
