import { toolRendererName, toolRootName } from './toolManifest.ts';

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
export function canRenderToolResult(
	toolName: string,
	content: string,
	resultRenderer: string | null = null,
): boolean {
	return parseToolResult(toolName, content, resultRenderer) !== null;
}

/**
 * Parse and classify a tool observation into a renderable payload. Dedicated
 * renderer selection remains in `toolResultRenderers.ts`; this module only
 * determines the stable result kind and preserves the decoded data.
 */
export function parseToolResult(
	toolName: string,
	content: string,
	resultRenderer: string | null = null,
): ParsedToolResult | null {
	// Root identifies the family for special terminal/notification handling.
	// Custom result cards are selected only by the backend renderer contract or
	// the current tool manifest; payload shape does not infer a legacy renderer.
	const rootToolName = toolRootName(toolName);
	// Empty content is still a shell card while streaming / waiting for the
	// first live-output chunk (or a background ToolRun bind).
	if (!content) {
		return toolName === 'shell' ? { kind: 'shell', data: null } : null;
	}
	if (rootToolName === 'shell') {
		let data: ToolResultObject | null = null;
		try {
			const value: unknown = JSON.parse(content);
			if (isObject(value)) data = value;
		} catch {
			// Plain text output — still renderable in the terminal card.
		}
		return { kind: 'shell', data };
	}
	if (rootToolName === 'notify' && content.startsWith('Notification sent:')) {
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
	if (resultRenderer || toolRendererName(toolName)) {
		return { kind: 'custom', data };
	}
	return { kind: 'generic', data };
}
