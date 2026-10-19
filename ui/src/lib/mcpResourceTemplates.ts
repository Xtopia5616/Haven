const OPERATORS = new Set(['+', '#', '.', '/', ';', '?', '&']);
const VARIABLE_NAME = /^[A-Za-z0-9_](?:[A-Za-z0-9._-]|%[0-9A-Fa-f]{2})*$/u;

interface OperatorConfig {
	prefix: string;
	separator: string;
	named: boolean;
	allowReserved: boolean;
}

const OPERATOR_CONFIG: Record<string, OperatorConfig> = {
	'': { prefix: '', separator: ',', named: false, allowReserved: false },
	'+': { prefix: '', separator: ',', named: false, allowReserved: true },
	'#': { prefix: '#', separator: ',', named: false, allowReserved: true },
	'.': { prefix: '.', separator: '.', named: false, allowReserved: false },
	'/': { prefix: '/', separator: '/', named: false, allowReserved: false },
	';': { prefix: ';', separator: ';', named: true, allowReserved: false },
	'?': { prefix: '?', separator: '&', named: true, allowReserved: false },
	'&': { prefix: '&', separator: '&', named: true, allowReserved: false },
};

export interface McpResourceTemplateVariables {
	template: string;
	variableNames: string[];
}

function parseExpression(expression: string): { operator: string; variables: string[] } {
	let operator = '';
	if (OPERATORS.has(expression[0] ?? '')) {
		operator = expression[0];
		expression = expression.slice(1);
	}
	if (!expression) throw new Error('资源模板包含空表达式');
	const variables = expression.split(',').map((specification) => {
		const match = /^(.+?)(?::([0-9]+)|(\*))?$/u.exec(specification);
		const name = match?.[1];
		if (!name || !VARIABLE_NAME.test(name)) throw new Error('资源模板包含无效变量名');
		if (match?.[2] !== undefined && Number(match[2]) > 10_000) {
			throw new Error('资源模板变量前缀超出限制');
		}
		return name;
	});
	return { operator, variables };
}

export function parseMcpResourceTemplate(template: string): McpResourceTemplateVariables {
	if (!template || new TextEncoder().encode(template).length > 8192) {
		throw new Error('资源模板为空或超出长度限制');
	}
	const variableNames: string[] = [];
	let cursor = 0;
	while (cursor < template.length) {
		const open = template.indexOf('{', cursor);
		const strayClose = template.indexOf('}', cursor);
		if (strayClose !== -1 && (open === -1 || strayClose < open)) {
			throw new Error('资源模板包含未配对的右花括号');
		}
		if (open === -1) break;
		const close = template.indexOf('}', open + 1);
		if (close === -1 || template.indexOf('{', open + 1) !== -1 && template.indexOf('{', open + 1) < close) {
			throw new Error('资源模板包含未配对的左花括号');
		}
		const parsed = parseExpression(template.slice(open + 1, close));
		variableNames.push(...parsed.variables);
		cursor = close + 1;
	}
	return { template, variableNames: [...new Set(variableNames)] };
}

function encodeValue(value: string, allowReserved: boolean): string {
	const unreserved = /^[A-Za-z0-9._~-]$/u;
	const reserved = ":/?#[]@!$&'()*+,;=";
	let encoded = '';
	for (const byte of new TextEncoder().encode(value)) {
		const character = String.fromCharCode(byte);
		if (byte < 0x80 && (unreserved.test(character) || (allowReserved && reserved.includes(character)))) {
			encoded += character;
		} else {
			encoded += `%${byte.toString(16).toUpperCase().padStart(2, '0')}`;
		}
	}
	return encoded;
}

function encodeVariableName(name: string): string {
	return name.replace(
		/%([0-9A-Fa-f]{2})|([^A-Za-z0-9._~-])/gu,
		(_match, escape: string | undefined, character: string | undefined) =>
			escape ? `%${escape.toUpperCase()}` : encodeValue(character ?? '', false),
	);
}

export function expandMcpResourceTemplate(
	template: string,
	values: Record<string, string>,
): string {
	const parsed = parseMcpResourceTemplate(template);
	if (Object.keys(values).some((name) => !parsed.variableNames.includes(name))) {
		throw new Error('资源模板参数包含未声明变量');
	}
	let result = '';
	let cursor = 0;
	while (cursor < template.length) {
		const open = template.indexOf('{', cursor);
		if (open === -1) {
			result += template.slice(cursor);
			break;
		}
		result += template.slice(cursor, open);
		const close = template.indexOf('}', open + 1);
		const expression = template.slice(open + 1, close);
		let operator = '';
		let variableExpression = expression;
		if (OPERATORS.has(expression[0] ?? '')) {
			operator = expression[0];
			variableExpression = expression.slice(1);
		}
		const config = OPERATOR_CONFIG[operator];
		const expanded: string[] = [];
		for (const specification of variableExpression.split(',')) {
			const match = /^(.+?)(?::([0-9]+)|(\*))?$/u.exec(specification);
			const name = match?.[1];
			if (!name || !Object.hasOwn(values, name)) continue;
			let value = values[name];
			if (match?.[2] !== undefined) value = [...value].slice(0, Number(match[2])).join('');
			const encoded = encodeValue(value, config.allowReserved);
			if (config.named) {
				const encodedName = encodeVariableName(name);
				expanded.push(`${encodedName}${value === '' && operator === ';' ? '' : `=${encoded}`}`);
			} else {
				expanded.push(encoded);
			}
		}
		if (expanded.length > 0) result += `${config.prefix}${expanded.join(config.separator)}`;
		cursor = close + 1;
	}
	if (!result || new TextEncoder().encode(result).length > 8192) {
		throw new Error('资源模板展开结果为空或超出长度限制');
	}
	return result;
}
