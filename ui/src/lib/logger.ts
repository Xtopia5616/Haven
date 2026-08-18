const LEVELS = ['debug', 'info', 'warn', 'error'];
const currentLevel = import.meta.env.DEV ? 'debug' : 'info';

function shouldLog(level) {
	return LEVELS.indexOf(level) >= LEVELS.indexOf(currentLevel);
}

const logger = {
	debug(context, msg, ...args) {
		if (shouldLog('debug')) {
			console.debug(`[DEBUG][${context}] ${msg}`, ...args);
		}
	},
	info(context, msg, ...args) {
		if (shouldLog('info')) {
			console.info(`[INFO][${context}] ${msg}`, ...args);
		}
	},
	warn(context, msg, ...args) {
		if (shouldLog('warn')) {
			console.warn(`[WARN][${context}] ${msg}`, ...args);
		}
	},
	error(context, msg, ...args) {
		if (shouldLog('error')) {
			console.error(`[ERROR][${context}] ${msg}`, ...args);
		}
	},
};

export default logger;
