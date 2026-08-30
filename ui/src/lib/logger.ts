const LEVELS = ['debug', 'info', 'warn', 'error'];
const currentLevel = import.meta.env.DEV ? 'debug' : 'info';

function shouldLog(level: string) {
	return LEVELS.indexOf(level) >= LEVELS.indexOf(currentLevel);
}

const logger = {
	debug(context: string, msg: string, ...args: unknown[]) {
		if (shouldLog('debug')) {
			console.debug(`[DEBUG][${context}] ${msg}`, ...args);
		}
	},
	info(context: string, msg: string, ...args: unknown[]) {
		if (shouldLog('info')) {
			console.info(`[INFO][${context}] ${msg}`, ...args);
		}
	},
	warn(context: string, msg: string, ...args: unknown[]) {
		if (shouldLog('warn')) {
			console.warn(`[WARN][${context}] ${msg}`, ...args);
		}
	},
	error(context: string, msg: string, ...args: unknown[]) {
		if (shouldLog('error')) {
			console.error(`[ERROR][${context}] ${msg}`, ...args);
		}
	},
};

export default logger;
