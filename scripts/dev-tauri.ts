import { spawn } from 'node:child_process';
import { homedir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';

const dataDirectory =
	process.platform === 'win32'
		? join(process.env.APPDATA ?? join(homedir(), 'AppData', 'Roaming'), 'haven-dev')
		: join(homedir(), '.local', 'share', 'haven-dev');

const child = spawn(
	'cargo',
	['tauri', 'dev', '--config', 'crates/app-binary/tauri.dev.conf.json', ...process.argv.slice(2)],
	{
		stdio: 'inherit',
		env: {
			...process.env,
			HAVEN_DATA_DIR: dataDirectory,
		},
	},
);

const forwardSignal = (signal: NodeJS.Signals) => {
	if (child.exitCode === null) child.kill(signal);
};
process.once('SIGINT', () => forwardSignal('SIGINT'));
process.once('SIGTERM', () => forwardSignal('SIGTERM'));

child.once('error', (error) => {
	console.error(`Failed to start Haven development app: ${error.message}`);
	process.exitCode = 1;
});

child.once('exit', (code, signal) => {
	process.exitCode = code ?? (signal ? 1 : 0);
});
