import { isRecord } from './objectGuards.ts';
import { isBoolean, isSafeInteger, isString } from './valueGuards.ts';
import { parseMcpResourceTemplate } from '$lib/mcpResourceTemplates.ts';

export type McpCacheScopeView = 'public' | 'private';

export interface McpResourceInfoView {
	uri: string;
	name: string;
	title?: string;
	description?: string;
	mimeType?: string;
	size?: number;
}

export interface McpResourceContentView {
	uri: string;
	mimeType?: string;
	text?: string;
	blob?: string;
}

export interface McpResourceListView {
	resources: McpResourceInfoView[];
	ttlMs: number | null;
	cacheScope: McpCacheScopeView;
}

export interface McpResourceTemplateView {
	uriTemplate: string;
	name: string;
	title?: string;
	description?: string;
	mimeType?: string;
}

export interface McpResourceTemplateListView {
	resourceTemplates: McpResourceTemplateView[];
	ttlMs: number | null;
	cacheScope: McpCacheScopeView;
}

export interface McpResourceReadView {
	contents: McpResourceContentView[];
	ttlMs: number | null;
	cacheScope: McpCacheScopeView;
}

export interface McpPromptArgumentView {
	name: string;
	title?: string;
	description?: string;
	required: boolean;
}

export interface McpPromptInfoView {
	name: string;
	title?: string;
	description?: string;
	arguments: McpPromptArgumentView[];
}

export interface McpPromptListView {
	prompts: McpPromptInfoView[];
	ttlMs: number | null;
	cacheScope: McpCacheScopeView;
}

export type McpPromptContentView =
	| { type: 'text'; text: string }
	| { type: 'image'; data: string; mimeType: string }
	| { type: 'audio'; data: string; mimeType: string }
	| {
			type: 'resource_link';
			uri: string;
			name: string;
			title?: string;
			description?: string;
			mimeType?: string;
			size?: number;
	  }
	| { type: 'resource'; resource: McpResourceContentView };

export interface McpPromptMessageView {
	role: 'user' | 'assistant';
	content: McpPromptContentView;
}

export interface McpPromptView {
	description: string | null;
	messages: McpPromptMessageView[];
	ttlMs: number | null;
	cacheScope: McpCacheScopeView;
}

export type McpFeatureCallStatusView = 'complete' | 'input_required';
export type McpElicitationFieldTypeView = 'string' | 'number' | 'integer' | 'boolean';

export interface McpElicitationFieldView {
	name: string;
	title: string | null;
	description: string | null;
	fieldType: McpElicitationFieldTypeView;
	required: boolean;
	options: string[];
	minimum: number | null;
	maximum: number | null;
	minLength: number | null;
	maxLength: number | null;
}

export interface McpElicitationRequestView {
	key: string;
	message: string;
	fields: McpElicitationFieldView[];
}

export interface McpFeatureCallOutcomeView {
	status: McpFeatureCallStatusView;
	resourceRead: McpResourceReadView | null;
	prompt: McpPromptView | null;
	requestId: number | null;
	round: number | null;
	requests: McpElicitationRequestView[];
}

export interface McpElicitationAnswerView {
	action: 'accept' | 'decline';
	content: Record<string, unknown>;
}

function optionalString(value: Record<string, unknown>, key: string, maxBytes: number): boolean {
	return (
		value[key] === undefined ||
		(isString(value[key]) && new TextEncoder().encode(value[key]).length <= maxBytes)
	);
}

function serializedBytesWithin(value: unknown, maxBytes: number): boolean {
	return new TextEncoder().encode(JSON.stringify(value)).length <= maxBytes;
}

function boundedText(value: unknown, maxBytes: number, allowEmpty = true): value is string {
	return (
		isString(value) &&
		(allowEmpty || value.length > 0) &&
		new TextEncoder().encode(value).length <= maxBytes &&
		![...value].some((character) =>
			/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(character),
		)
	);
}

function isCacheScope(value: unknown): value is McpCacheScopeView {
	return value === 'public' || value === 'private';
}

function isTtl(value: unknown): value is number | null {
	return value === null || (isSafeInteger(value) && value >= 0);
}

function optionalSize(value: unknown): boolean {
	return value === undefined || (isSafeInteger(value) && value >= 0);
}

function isResourceInfo(value: unknown): value is McpResourceInfoView {
	return (
		isRecord(value) &&
		boundedText(value.uri, 8192, false) &&
		boundedText(value.name, 1024, false) &&
		optionalString(value, 'title', 1024) &&
		optionalString(value, 'description', 16 * 1024) &&
		optionalString(value, 'mimeType', 512) &&
		optionalSize(value.size)
	);
}

function isResourceContent(value: unknown): value is McpResourceContentView {
	return (
		isRecord(value) &&
		boundedText(value.uri, 8192, false) &&
		optionalString(value, 'mimeType', 512) &&
		((boundedText(value.text, 1024 * 1024) && value.blob === undefined) ||
			(value.text === undefined &&
				isString(value.blob) &&
				value.blob.length <= 3 * 1024 * 1024))
	);
}

function isResourceList(value: unknown): value is McpResourceListView {
	return (
		isRecord(value) &&
		Array.isArray(value.resources) &&
		value.resources.length <= 10_000 &&
		value.resources.every(isResourceInfo) &&
		new Set(value.resources.map((resource) => resource.uri)).size === value.resources.length &&
		serializedBytesWithin(value.resources, 4 * 1024 * 1024) &&
		isTtl(value.ttlMs) &&
		isCacheScope(value.cacheScope)
	);
}

export function parseMcpResourceList(value: unknown): McpResourceListView {
	if (!isResourceList(value)) throw new Error('Invalid MCP resource list');
	return value;
}

function isResourceTemplate(value: unknown): value is McpResourceTemplateView {
	if (
		!isRecord(value) ||
		!boundedText(value.uriTemplate, 8192, false) ||
		!boundedText(value.name, 1024, false) ||
		!optionalString(value, 'title', 1024) ||
		!optionalString(value, 'description', 16 * 1024) ||
		!optionalString(value, 'mimeType', 512)
	)
		return false;
	try {
		parseMcpResourceTemplate(value.uriTemplate);
		return true;
	} catch {
		return false;
	}
}

function isResourceTemplateList(value: unknown): value is McpResourceTemplateListView {
	return (
		isRecord(value) &&
		Array.isArray(value.resourceTemplates) &&
		value.resourceTemplates.length <= 10_000 &&
		value.resourceTemplates.every(isResourceTemplate) &&
		new Set(value.resourceTemplates.map((template) => template.uriTemplate)).size ===
			value.resourceTemplates.length &&
		serializedBytesWithin(value.resourceTemplates, 4 * 1024 * 1024) &&
		isTtl(value.ttlMs) &&
		isCacheScope(value.cacheScope)
	);
}

export function parseMcpResourceTemplateList(value: unknown): McpResourceTemplateListView {
	if (!isResourceTemplateList(value)) throw new Error('Invalid MCP resource template list');
	return value;
}

function isResourceRead(value: unknown): value is McpResourceReadView {
	return (
		isRecord(value) &&
		Array.isArray(value.contents) &&
		value.contents.length <= 64 &&
		value.contents.every(isResourceContent) &&
		serializedBytesWithin(value.contents, 4 * 1024 * 1024) &&
		isTtl(value.ttlMs) &&
		isCacheScope(value.cacheScope)
	);
}

export function parseMcpResourceRead(value: unknown): McpResourceReadView {
	if (!isResourceRead(value)) throw new Error('Invalid MCP resource response');
	return value;
}

function isPromptArgument(value: unknown): value is McpPromptArgumentView {
	return (
		isRecord(value) &&
		boundedText(value.name, 1024, false) &&
		optionalString(value, 'title', 1024) &&
		optionalString(value, 'description', 16 * 1024) &&
		isBoolean(value.required)
	);
}

function isPromptInfo(value: unknown): value is McpPromptInfoView {
	return (
		isRecord(value) &&
		boundedText(value.name, 1024, false) &&
		optionalString(value, 'title', 1024) &&
		optionalString(value, 'description', 16 * 1024) &&
		Array.isArray(value.arguments) &&
		value.arguments.length <= 128 &&
		value.arguments.every(isPromptArgument) &&
		new Set(value.arguments.map((argument) => argument.name)).size === value.arguments.length
	);
}

function isPromptList(value: unknown): value is McpPromptListView {
	return (
		isRecord(value) &&
		Array.isArray(value.prompts) &&
		value.prompts.length <= 10_000 &&
		value.prompts.every(isPromptInfo) &&
		new Set(value.prompts.map((prompt) => prompt.name)).size === value.prompts.length &&
		serializedBytesWithin(value.prompts, 4 * 1024 * 1024) &&
		isTtl(value.ttlMs) &&
		isCacheScope(value.cacheScope)
	);
}

export function parseMcpPromptList(value: unknown): McpPromptListView {
	if (!isPromptList(value)) throw new Error('Invalid MCP prompt list');
	return value;
}

function isPromptContent(value: unknown): value is McpPromptContentView {
	if (!isRecord(value) || !isString(value.type)) return false;
	switch (value.type) {
		case 'text':
			return boundedText(value.text, 512 * 1024);
		case 'image':
		case 'audio':
			return (
				boundedText(value.data, 720 * 1024, false) &&
				boundedText(value.mimeType, 512, false)
			);
		case 'resource_link':
			return (
				boundedText(value.uri, 8192, false) &&
				boundedText(value.name, 1024, false) &&
				optionalString(value, 'title', 1024) &&
				optionalString(value, 'description', 16 * 1024) &&
				optionalString(value, 'mimeType', 512) &&
				optionalSize(value.size)
			);
		case 'resource':
			return isResourceContent(value.resource);
		default:
			return false;
	}
}

function isPromptMessage(value: unknown): value is McpPromptMessageView {
	return (
		isRecord(value) &&
		(value.role === 'user' || value.role === 'assistant') &&
		isPromptContent(value.content)
	);
}

function isPrompt(value: unknown): value is McpPromptView {
	return (
		isRecord(value) &&
		(value.description === null ||
			(isString(value.description) &&
				new TextEncoder().encode(value.description).length <= 16 * 1024)) &&
		Array.isArray(value.messages) &&
		value.messages.length > 0 &&
		value.messages.length <= 128 &&
		value.messages.every(isPromptMessage) &&
		new TextEncoder().encode(JSON.stringify(value)).length <= 512 * 1024 &&
		isTtl(value.ttlMs) &&
		isCacheScope(value.cacheScope)
	);
}

export function parseMcpPrompt(value: unknown): McpPromptView {
	if (!isPrompt(value)) throw new Error('Invalid MCP prompt response');
	return value;
}

function isElicitationField(value: unknown): value is McpElicitationFieldView {
	return (
		isRecord(value) &&
		boundedText(value.name, 256, false) &&
		(value.title === null || boundedText(value.title, 4096)) &&
		(value.description === null || boundedText(value.description, 4096)) &&
		(value.fieldType === 'string' ||
			value.fieldType === 'number' ||
			value.fieldType === 'integer' ||
			value.fieldType === 'boolean') &&
		isBoolean(value.required) &&
		Array.isArray(value.options) &&
		value.options.length <= 64 &&
		value.options.every((item) => boundedText(item, 16 * 1024, false)) &&
		(value.minimum === null ||
			(typeof value.minimum === 'number' && Number.isFinite(value.minimum))) &&
		(value.maximum === null ||
			(typeof value.maximum === 'number' && Number.isFinite(value.maximum))) &&
		(value.minLength === null || (isSafeInteger(value.minLength) && value.minLength >= 0)) &&
		(value.maxLength === null || (isSafeInteger(value.maxLength) && value.maxLength >= 0))
	);
}

function isElicitationRequest(value: unknown): value is McpElicitationRequestView {
	return (
		isRecord(value) &&
		boundedText(value.key, 256, false) &&
		boundedText(value.message, 4096) &&
		Array.isArray(value.fields) &&
		value.fields.length <= 32 &&
		value.fields.every(isElicitationField) &&
		serializedBytesWithin(value, 64 * 1024)
	);
}

function isFeatureCallOutcome(value: unknown): value is McpFeatureCallOutcomeView {
	if (!isRecord(value) || (value.status !== 'complete' && value.status !== 'input_required'))
		return false;
	const validRead = value.resourceRead === null || isResourceRead(value.resourceRead);
	const validPrompt = value.prompt === null || isPrompt(value.prompt);
	const validRequestId =
		value.requestId === null || (isSafeInteger(value.requestId) && value.requestId >= 0);
	const validRound =
		value.round === null || (isSafeInteger(value.round) && value.round > 0 && value.round <= 8);
	if (
		!validRead ||
		!validPrompt ||
		!validRequestId ||
		!validRound ||
		!Array.isArray(value.requests) ||
		value.requests.length > 8 ||
		!value.requests.every(isElicitationRequest)
	)
		return false;
	if (value.status === 'complete') {
		return (
			value.requestId === null &&
			value.round === null &&
			value.requests.length === 0 &&
			(value.resourceRead !== null) !== (value.prompt !== null)
		);
	}
	return (
		value.resourceRead === null &&
		value.prompt === null &&
		value.requestId !== null &&
		value.round !== null &&
		serializedBytesWithin(value, 256 * 1024)
	);
}

export function parseMcpFeatureCallOutcome(value: unknown): McpFeatureCallOutcomeView {
	if (!isFeatureCallOutcome(value)) throw new Error('Invalid MCP feature interaction response');
	return value;
}
