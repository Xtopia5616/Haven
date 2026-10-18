import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { JSDOM } from 'jsdom';

const indexPath = new URL('../build/index.html', import.meta.url);
const manifestPath = new URL('../build/.haven-csp-hashes.json', import.meta.url);
const html = await readFile(indexPath, 'utf8');
const document = new JSDOM(html).window.document;
const hashes = [...document.querySelectorAll('script:not(:empty)')].map((script) => {
	const normalized = (script.textContent ?? '').replace(/\r\n?/g, '\n');
	const digest = createHash('sha256').update(normalized, 'utf8').digest('base64');
	return `'sha256-${digest}'`;
});

await writeFile(manifestPath, JSON.stringify(hashes), 'utf8');
console.log(`Wrote CSP hashes for ${hashes.length} inline script(s).`);
