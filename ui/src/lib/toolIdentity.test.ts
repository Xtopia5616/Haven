import { describe, it, expect } from 'vitest';
import {
	classifyToolSource,
	parseToolArgs,
	toolDisplayName,
	toolSourceLabel,
} from './toolIdentity.ts';
import { setToolManifests } from './toolManifest.ts';

function manifest(stableName: string, root: string, label: string) {
	return {
		identity: {
			source: 'builtin',
			catalog_group: 'system',
			root,
			operation: stableName.split('.').at(-1) ?? null,
			stable_name: stableName,
		},
		model: { name: stableName, description: label, input_schema: { type: 'object' } },
		policy: {
			risk_level: 'safe',
			permission_key: stableName,
			confirmation: 'none',
			idempotency: 'idempotent',
			scope: 'session',
			concurrency: 'read_only',
			effect: 'read_only',
			data_sensitivity: 'none',
			network_access: 'none',
		},
		presentation: { label, renderer: root, icon: 'tools', represented_source: 'builtin' },
		root_presentation: { label: root, description: `${root} capabilities`, icon: 'tools' },
		prompt: { when_to_use: 'use', when_not_to_use: 'never', key_operations: [stableName] },
		availability: {
			enabled: true,
			available: true,
			availability_reason: null,
			requires_connection: false,
			requires_permission: false,
		},
	};
}

describe('classifyToolSource', () => {
	it('detects mcp and skill wire prefixes', () => {
		expect(classifyToolSource('mcp__filesystem__read')).toBe('mcp');
		expect(classifyToolSource('skill__weather')).toBe('skill');
		expect(classifyToolSource('mcp_filesystem_read')).toBe('builtin');
		expect(classifyToolSource('skill_weather')).toBe('builtin');
	});

	it('classifies MCP and skill activation tools by the capability they load', () => {
		expect(classifyToolSource('load_mcp')).toBe('mcp');
		expect(classifyToolSource('skill__weather')).toBe('skill');
	});

	it('treats regular builtins as builtin', () => {
		expect(classifyToolSource('shell')).toBe('builtin');
		expect(classifyToolSource('')).toBe('builtin');
	});
});

describe('toolSourceLabel / toolDisplayName', () => {
	it('returns badge labels', () => {
		expect(toolSourceLabel('mcp')).toBe('MCP');
		expect(toolSourceLabel('skill')).toBe('Skill');
		expect(toolSourceLabel('builtin')).toBe('内置');
	});

	it('uses Chinese labels for builtins and strips mcp/skill prefixes', () => {
		expect(toolDisplayName('shell')).toBe('终端输出');
		expect(toolDisplayName('load_mcp')).toBe('加载 MCP');
		expect(toolDisplayName('mcp__test-server__greet')).toBe('test-server__greet');
		expect(toolDisplayName('skill__echo')).toBe('echo');
		expect(toolDisplayName('mcp_filesystem_read')).toBe('mcp_filesystem_read');
		expect(toolDisplayName('skill_weather')).toBe('skill_weather');
		expect(toolDisplayName('mcp::fs::read')).toBe('mcp::fs::read');
	});

	it('uses stable labels for operation views', () => {
		setToolManifests([
			manifest('files.read', 'files', '读取文件'),
			manifest('files.search', 'files', '搜索文件'),
			manifest('system.info', 'system', '系统信息'),
		]);
		expect(toolDisplayName('files.read')).toBe('读取文件');
		expect(toolDisplayName('files.search')).toBe('搜索文件');
		expect(toolDisplayName('system.info')).toBe('系统信息');
	});
});

describe('parseToolArgs', () => {
	it('returns null for empty input', () => {
		expect(parseToolArgs(null)).toBeNull();
		expect(parseToolArgs(undefined)).toBeNull();
		expect(parseToolArgs('')).toBeNull();
		expect(parseToolArgs('   ')).toBeNull();
	});

	it('passes objects through and parses JSON strings', () => {
		expect(parseToolArgs({ path: 'a.rs' })).toEqual({ path: 'a.rs' });
		expect(parseToolArgs('{"path":"a.rs"}')).toEqual({ path: 'a.rs' });
		expect(parseToolArgs('not-json')).toEqual({ raw: 'not-json' });
		expect(parseToolArgs(42)).toEqual({ value: 42 });
	});
});
