const LEVELS = ['debug', 'info', 'warn', 'error'];
const currentLevel = import.meta.env.DEV ? 'debug' : 'info';

function normalizeContext(context: string): string {
	return context.trim().replace(/\s+/g, '_') || 'unknown';
}

function write(level: string, context: string, msg: string) {
	const prefix = `[${level.toUpperCase()}][${normalizeContext(context)}]`;
	const method =
		level === 'debug'
			? console.debug
			: level === 'info'
				? console.info
				: level === 'warn'
					? console.warn
					: console.error;
	method(`${prefix} ${msg}`);
}

function shouldLog(level: string) {
	return LEVELS.indexOf(level) >= LEVELS.indexOf(currentLevel);
}

const logger = {
	debug(context: string, msg: string) {
		if (shouldLog('debug')) {
			write('debug', context, msg);
		}
	},
	info(context: string, msg: string) {
		if (shouldLog('info')) {
			write('info', context, msg);
		}
	},
	warn(context: string, msg: string) {
		if (shouldLog('warn')) {
			write('warn', context, msg);
		}
	},
	error(context: string, msg: string) {
		if (shouldLog('error')) {
			write('error', context, msg);
		}
	},
};

export default logger;
