import { isRecord } from './objectGuards.ts';
import { isBoolean, isSafeInteger, isString } from './valueGuards.ts';

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

function optionalString(value: Record<string, unknown>, key: string, maxBytes: number): boolean {
	return value[key] === undefined || (isString(value[key]) && new TextEncoder().encode(value[key]).length <= maxBytes);
}

function serializedBytesWithin(value: unknown, maxBytes: number): boolean {
	return new TextEncoder().encode(JSON.stringify(value)).length <= maxBytes;
}

function boundedText(value: unknown, maxBytes: number, allowEmpty = true): value is string {
	return (
		isString(value) &&
		(allowEmpty || value.length > 0) &&
		new TextEncoder().encode(value).length <= maxBytes &&
		![...value].some((character) => /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(character))
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
			(value.text === undefined && isString(value.blob) && value.blob.length <= 3 * 1024 * 1024))
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
			return boundedText(value.data, 720 * 1024, false) && boundedText(value.mimeType, 512, false);
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
			(isString(value.description) && new TextEncoder().encode(value.description).length <= 16 * 1024)) &&
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
