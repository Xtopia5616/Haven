import { spawn } from 'node:child_process';
import { createConnection } from 'node:net';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const UI_DIR = resolve(SCRIPT_DIR, '..', 'ui');
const VITE_BIN = resolve(UI_DIR, 'node_modules', 'vite', 'bin', 'vite.js');
const UI_MARKER = UI_DIR.replaceAll('\\', '/').toLowerCase();
const PORT = 4721;
const ORIGINS = [`http://localhost:${PORT}`, `http://127.0.0.1:${PORT}`];
const PROBE_TIMEOUT_MS = 800;
const SERVER_FAILURE_LIMIT = 3;

function wait(milliseconds) {
	return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

async function fetchText(url) {
	const controller = new AbortController();
	const timeout = setTimeout(() => controller.abort(), PROBE_TIMEOUT_MS);
	try {
		const response = await fetch(url, { signal: controller.signal });
		return response.ok ? await response.text() : null;
	} catch {
		return null;
	} finally {
		clearTimeout(timeout);
	}
}

async function isHavenViteServer(origin) {
	const html = await fetchText(`${origin}/`);
	if (
		!html ||
		!html.includes('<title>Haven</title>') ||
		!html.includes('__sveltekit_dev') ||
		!html.replaceAll('\\', '/').toLowerCase().includes(UI_MARKER)
	) {
		return false;
	}
	return (await fetchText(`${origin}/@vite/client`)) !== null;
}

function isPortOpen(host, port) {
	return new Promise((resolvePromise) => {
		const socket = createConnection({ host, port });
		let finished = false;
		const finish = (open) => {
			if (finished) return;
			finished = true;
			socket.destroy();
			resolvePromise(open);
		};
		socket.once('connect', () => finish(true));
		socket.once('error', () => finish(false));
		socket.setTimeout(PROBE_TIMEOUT_MS, () => finish(false));
	});
}

async function findExistingServer() {
	for (const origin of ORIGINS) {
		if (await isHavenViteServer(origin)) return { kind: 'haven', origin };
	}

	const portOpen =
		(await isPortOpen('127.0.0.1', PORT)) || (await isPortOpen('::1', PORT));
	return portOpen ? { kind: 'other' } : null;
}

async function waitForExistingServer(origin) {
	let failures = 0;
	console.log(`Reusing the existing Haven Vite server at ${origin}.`);
	while (failures < SERVER_FAILURE_LIMIT) {
		if (await isHavenViteServer(origin)) {
			failures = 0;
		} else {
			failures += 1;
		}
		await wait(1000);
	}
	console.error('The reused Haven Vite server stopped.');
	process.exitCode = 1;
}

function forwardedViteArgs() {
	const args = process.argv.slice(2);
	return args[0] === '--' ? args.slice(1) : args;
}

async function main() {
	const existing = await findExistingServer();
	if (existing?.kind === 'haven') {
		await waitForExistingServer(existing.origin);
		return;
	}
	if (existing?.kind === 'other') {
		console.error(
			`Port ${PORT} is already used by another service. Stop that service or close the existing Haven UI before starting Vite.`,
		);
		process.exitCode = 1;
		return;
	}

	const vite = spawn(process.execPath, [VITE_BIN, 'dev', ...forwardedViteArgs()], {
		cwd: UI_DIR,
		stdio: 'inherit',
		windowsHide: false,
	});

	const forwardSignal = (signal) => {
		if (vite.exitCode === null) vite.kill(signal);
	};
	process.once('SIGINT', () => forwardSignal('SIGINT'));
	process.once('SIGTERM', () => forwardSignal('SIGTERM'));
	vite.once('error', (error) => {
		console.error(`Could not start Vite: ${error.message}`);
		process.exitCode = 1;
	});
	vite.once('exit', (code, signal) => {
		process.exitCode = code ?? (signal ? 1 : 0);
	});
}

main().catch((error) => {
	console.error(`Could not prepare the Vite dev server: ${error.message}`);
	process.exitCode = 1;
});
