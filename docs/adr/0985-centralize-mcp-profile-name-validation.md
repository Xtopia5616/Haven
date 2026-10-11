# ADR 0985：统一 MCP profile 名称校验

## 状态

已采纳并实施。

## 背景

MCP server profile name 是 Common 配置和管理操作使用的精确身份。UI 编辑器曾单独限制名称为 1–128 位 ASCII 字母、数字、`-`、`_`，但配置加载、ConfigService 写入和应用管理入口没有共享后端规则；手工配置可绕过 UI 并引入点号等名称，造成列表键、管理查找和 provider 工具名前缀语义分歧。

## 决定

1. Common config owner 定义唯一 Rust validator：profile 名称长度 1–128 字节，只接受 ASCII 字母、数字、`-`、`_`；同一配置列表中的名称按精确、大小写敏感身份去重，不 trim、不改写。
2. ConfigLoader 语义加载与保存、ConfigService 构造和 patch 写入都执行 validator。ConfigService 的失败写入回滚内存快照。
3. UI 编辑器调用单一 `isValidMcpServerName`，与 Rust 规则保持一致；后端仍是强制边界。
4. 既有 ConfigLoader 行为不变：无效配置在加载时保留 timestamped backup，并以默认配置继续运行；既有名称不会被自动改名。
5. MCP 协议返回的 raw tool name 继续按远端协议处理，本 ADR 只约束 Haven-owned server profile identity。

## 影响与验证

- 非法或重复 profile name 无法进入运行配置或写入 config.toml。
- `Foo` 与 `foo` 仍是不同身份；现有 `mcp_add` 同名 upsert 语义保持不变。
- ConfigLoader、ConfigService 和 UI validator 单测覆盖长度、字符集、重复名及大小写行为。

## 回滚

移除 Common 和 UI validator 调用、恢复 UI 局部正则，并删除对应测试与文档说明。无效配置继续按旧路径加载；无需数据库迁移。
