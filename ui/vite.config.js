import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';
import { svelteTesting } from '@testing-library/svelte/vite';

export default defineConfig({
	plugins: [
		sveltekit(),
		...(Boolean(/** @type {any} */ (globalThis).process?.env?.VITEST)
			? [svelteTesting()]
			: [])
	],
	server: {
		port: 4721,
		strictPort: true
	},
	build: {
		target: 'es2022'
	},
	test: {
		include: ['src/**/*.{test,spec}.{js,ts}'],
		environment: 'jsdom',
		setupFiles: ['src/test-setup.ts'],
		coverage: {
			provider: 'v8',
			reporter: ['text', 'html'],
			include: ['src/**'],
			exclude: ['src/**/*.{test,spec}.{js,ts}', 'src/test-setup.ts']
		}
	}
});
