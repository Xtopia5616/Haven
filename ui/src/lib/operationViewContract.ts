/**
 * Model-facing operation views have stable UI metadata as well as a stable
 * backend name. Rust owns execution policy; this mirror owns the renderer
 * boundary and is checked by tests against every advertised view name.
 */
export type OperationViewContract = {
	root: string;
	label: string;
	renderer: string;
	icon: string;
	prompt: string;
};

export const OPERATION_VIEW_CONTRACTS: Record<string, OperationViewContract> = {
	'files.read_text': {
		root: 'files',
		label: '读取文件',
		renderer: 'files',
		icon: 'file',
		prompt: '读取文本文件；使用 offset/limit 或 start_line/end_line 继续读取截断内容。',
	},
	'files.outline': {
		root: 'files',
		label: '文件大纲',
		renderer: 'files',
		icon: 'fileSearch',
		prompt: '先查看源文件结构；使用 next_page.start_line 继续读取下一页。',
	},
	'files.summary': {
		root: 'files',
		label: '文件摘要',
		renderer: 'files',
		icon: 'file',
		prompt: '对文本文件或指定行范围生成摘要；不要把摘要当作原文。',
	},
	'files.search': {
		root: 'files',
		label: '搜索文件',
		renderer: 'files.search',
		icon: 'search',
		prompt: '搜索结果包含路径、行号和上下文；需要完整内容时再调用 files.read_text。',
	},
	'system.info': {
		root: 'system',
		label: '系统信息',
		renderer: 'system',
		icon: 'cpu',
		prompt: '读取受限的机器信息快照；category 用于缩小返回范围。',
	},
};

export function operationViewContract(toolName: string): OperationViewContract | null {
	return OPERATION_VIEW_CONTRACTS[toolName] ?? null;
}

export function toolRootName(toolName: string): string {
	return operationViewContract(toolName)?.root ?? toolName;
}

export function toolIconName(toolName: string): string | null {
	return operationViewContract(toolName)?.icon ?? null;
}
