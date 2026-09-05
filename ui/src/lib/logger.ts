const LEVELS = ['debug', 'info', 'warn', 'error'];
const currentLevel = import.meta.env.DEV ? 'debug' : 'info';

function normalizeContext(context: string): string {
	return context.trim().replace(/\s+/g, '_') || 'unknown';
}

function write(level: string, context: string, msg: string, args: unknown[]) {
	const prefix = `[${level.toUpperCase()}][${normalizeContext(context)}]`;
	const method =
		level === 'debug'
			? console.debug
			: level === 'info'
				? console.info
				: level === 'warn'
					? console.warn
					: console.error;
	method(`${prefix} ${msg}`, ...args);
}

function shouldLog(level: string) {
	return LEVELS.indexOf(level) >= LEVELS.indexOf(currentLevel);
}

const logger = {
	debug(context: string, msg: string, ...args: unknown[]) {
		if (shouldLog('debug')) {
			write('debug', context, msg, args);
		}
	},
	info(context: string, msg: string, ...args: unknown[]) {
		if (shouldLog('info')) {
			write('info', context, msg, args);
		}
	},
	warn(context: string, msg: string, ...args: unknown[]) {
		if (shouldLog('warn')) {
			write('warn', context, msg, args);
		}
	},
	error(context: string, msg: string, ...args: unknown[]) {
		if (shouldLog('error')) {
			write('error', context, msg, args);
		}
	},
};

export default logger;
