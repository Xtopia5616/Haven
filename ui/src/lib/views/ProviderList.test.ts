import { describe, expect, it } from 'vitest';
import providerListSource from './ProviderList.svelte?raw';

describe('ProviderList responsive layout', () => {
	it('uses the settings content width for its narrow toolbar breakpoints', () => {
		expect(providerListSource).toContain('@container settings-content (max-width: 700px)');
		expect(providerListSource).toContain('@container settings-content (max-width: 455px)');
	});
});
