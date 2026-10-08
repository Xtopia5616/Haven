import { describe, expect, it } from 'vitest';
import providerListSource from './ProviderList.svelte?raw';

describe('ProviderList responsive layout', () => {
	it('keeps the provider toolbar within its available content width', () => {
		expect(providerListSource).toMatch(/\.provider-toolbar\s*\{[^}]*flex-wrap:\s*nowrap;/s);
		expect(providerListSource).toMatch(
			/\.provider-toolbar-actions\s*\{[^}]*flex-wrap:\s*nowrap;[^}]*flex:\s*0 1 auto;[^}]*max-width:\s*100%;[^}]*translateX\(calc\(0px - var\(--md-sys-space-2xl\)\)\);/s,
		);
		expect(providerListSource).toContain('white-space: nowrap;');
		expect(providerListSource).not.toContain('flex-direction: column-reverse;');
		expect(providerListSource).not.toMatch(
			/@container settings-content \(max-width: 700px\)\s*\{\s*\.provider-toolbar\s*\{[^}]*flex-direction:\s*column;/s,
		);
		expect(providerListSource).toContain('@container settings-content (max-width: 700px)');
		expect(providerListSource).toContain('@container settings-content (max-width: 455px)');
	});
});
