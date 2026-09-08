import { describe, it, expect } from 'vitest';
import {
	classifyToolSource,
	parseToolArgs,
	toolDisplayName,
	toolSourceLabel,
} from './toolIdentity.ts';

describe('classifyToolSource', () => {
	it('detects mcp and skill wire prefixes', () => {
		expect(classifyToolSource('mcp__filesystem__read')).toBe('mcp');
		expect(classifyToolSource('skill__weather')).toBe('skill');
		expect(classifyToolSource('mcp_filesystem_read')).toBe('builtin');
		expect(classifyToolSource('skill_weather')).toBe('builtin');
	});

	it('treats builtins and load_* meta-tools as builtin', () => {
		expect(classifyToolSource('shell')).toBe('builtin');
		expect(classifyToolSource('load_mcp')).toBe('builtin');
		expect(classifyToolSource('load_skill')).toBe('builtin');
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
		expect(toolDisplayName('haven_config')).toBe('Haven 配置');
		expect(toolDisplayName('haven_session_diagnostics')).toBe('会话诊断');
		expect(toolDisplayName('haven')).toBe('haven');
		expect(toolDisplayName('load_mcp')).toBe('加载 MCP');
		expect(toolDisplayName('mcp__test-server__greet')).toBe('test-server__greet');
		expect(toolDisplayName('skill__echo')).toBe('echo');
		expect(toolDisplayName('mcp_filesystem_read')).toBe('mcp_filesystem_read');
		expect(toolDisplayName('skill_weather')).toBe('skill_weather');
		expect(toolDisplayName('mcp::fs::read')).toBe('mcp::fs::read');
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
