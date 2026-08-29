type ToolResultObject = Record<string, any>;

export type ParsedToolResult = {
	kind: 'custom' | 'generic' | 'shell' | 'notify' | 'raw';
	data: unknown;
};

/** @param value unknown JSON-like value */
function isObject(value: unknown): value is ToolResultObject {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Whether this tool observation can be rendered as a card. Every non-empty
 * observation is renderable — structured JSON gets its dedicated renderer
 * and anything else falls back to the `raw` card — so this is only false
 * for empty content.
 */
export function canRenderToolResult(toolName: string, content: string): boolean {
	return parseToolResult(toolName, content) !== null;
}

/**
 * Parse and classify a tool observation into a renderable payload. Dedicated
 * renderer selection remains in `toolResultRenderers.ts`; this module only
 * determines the stable result kind and preserves the decoded data.
 */
export function parseToolResult(toolName: string, content: string): ParsedToolResult | null {
	// Empty content is still a shell card while streaming / waiting for the
	// first live-output chunk (or a background action bind).
	if (!content) {
		return toolName === 'shell' ? { kind: 'shell', data: null } : null;
	}
	if (toolName === 'shell') {
		let data: ToolResultObject | null = null;
		try {
			const value: unknown = JSON.parse(content);
			if (isObject(value)) data = value;
		} catch {
			// Plain text output — still renderable in the terminal card.
		}
		return { kind: 'shell', data };
	}
	if (toolName === 'notify' && content.startsWith('Notification sent:')) {
		return { kind: 'notify', data: null };
	}

	let data: unknown;
	try {
		data = JSON.parse(content);
	} catch {
		// Not JSON — plain text, rendered in the raw card.
		return { kind: 'raw', data: null };
	}
	if (!isObject(data)) {
		// JSON arrays / primitives — pretty-printed in the raw card.
		return { kind: 'raw', data };
	}
	return customShape(toolName, data)
		? { kind: 'custom', data }
		: { kind: 'generic', data };
}

/** Match a JSON observation against a dedicated renderer shape. */
function customShape(toolName: string, data: ToolResultObject): ToolResultObject | null {
	switch (toolName) {
		case 'file_search':
		case 'files':
			if (Array.isArray(data.results)) return data;
			if (
				data.written ||
				data.edited ||
				data.copied ||
				data.moved ||
				data.deleted ||
				Array.isArray(data.entries) ||
				'content' in data ||
				'size' in data
			)
				return data;
			return null;
		case 'system':
			return data.cpu ||
				data.memory ||
				data.os ||
				data.disks ||
				Array.isArray(data.displays) ||
				Array.isArray(data.variables) ||
				data.name ||
				'battery_percent' in data ||
				data.locked ||
				data.sleep ||
				data.hibernate
				? data
				: null;
		case 'process':
			return Array.isArray(data.processes) ? data : null;
		case 'window':
			return Array.isArray(data.windows) ||
				Array.isArray(data.elements) ||
				typeof data.text === 'string' ||
				data.waited === true
				? data
				: null;
		case 'actions':
			return Array.isArray(data.actions) ||
				typeof data.status === 'string' ||
				data.operation === 'result_injected'
				? data
				: null;
		case 'schedule':
			return Array.isArray(data.scheduled_actions) || (data.id && data.mode) ? data : null;
		case 'file':
			return data.written ||
				data.edited ||
				data.copied ||
				data.moved ||
				data.deleted ||
				Array.isArray(data.entries) ||
				'content' in data ||
				'size' in data
				? data
				: null;
		case 'http':
			return typeof data.status === 'number' ? data : null;
		case 'clipboard':
			return 'content' in data || data.written === true ? data : null;
		case 'web_search':
			// Provider built-in web search tool return: `{label, queries,
			// results:[{title,url,snippet}]}` composed by the page handler.
			return (Array.isArray(data.results) || Array.isArray(data.queries)) &&
				typeof data.label === 'string'
				? data
				: null;
		case 'agent':
			return data.operation ||
				data.ok === true ||
				data.ok === false ||
				Array.isArray(data.agents) ||
				data.session_id ||
				data.timed_out === true ||
				data.auto === true ||
				typeof data.text === 'string' ||
				data.reply ||
				data.message_id
				? data
				: null;
		default:
			return null;
	}
}
