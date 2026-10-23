# ADR 1383：拆分 MCP 可用性、目录展示和调用适配

## 状态

已实施 — 2026-10-23。完成执行上下文 Slice 4；Code Mode 子调用继续复用现有授权、MCP 远程目标契约与 durable journal。

## 背景

`McpToolExposure` 的 `direct` / `model_only` / `deferred` / `codemode` / `hidden` 同时表达是否允许调用、schema 的发现方式和执行入口。目录、`load_mcp`、provider-native MCP tool surface、Code Mode 子调用和 stale overlay 校验都需要解释同一模式；server 级 Legacy/CodeMode 切换还把独立决策叠在一起。Code Mode 已有独立 bounded adapter，且每个子调用已通过普通 Tools 授权、MCP typed remote resource 和 durable journal；本切片不新建授权 owner。

## 决定

1. 每项 MCP 工具配置保存三个独立维度：
   - `availability`: `enabled` / `disabled`；只有启用项可经 adapter 路由。
   - `catalog`: `direct` / `deferred` / `hidden`；决定直接展示、按需发现/加载，或仅从通用目录隐藏。
   - `invocation`: `none` / `native` / `codemode` / `native_and_codemode`；决定可用的执行适配器。
2. 原生 provider surface 要求 `enabled + native adapter`，且 catalog 不能是 `deferred`；`direct` 和 `hidden` 的 native adapter 都直接进入 provider surface，区别仅是 `hidden` 不出现在通用目录。`load_mcp` 和 deferred session overlay 要求 `enabled + deferred + native adapter`。Code Mode 子调用要求 `enabled + codemode adapter`，不以目录状态代替调用授权。需要阻止调用时必须禁用能力。无 adapter 的工具不会被发到调用 surface。
3. 两种 adapter 可以同时启用；它们都进入现有 `ToolAuthorizationContract`、同一 typed MCP 远程资源和 Code Mode journal，不因调用通道增加第二份 grant、target 或重试状态。
4. 新配置默认 `enabled + deferred + native`。per-tool exact/wildcard resolver 保留精确匹配优先、最长字面 pattern 优先和稳定字典序 tie-break。
5. 配置加载时做一次保守迁移并写回新对象形状：
   - `direct` / `model_only` → `enabled + direct + native`。当前实现中两值都只进入原生 surface，且都不进入 Code Mode 子调用，因此转换后权限等强。
   - `deferred` → `enabled + deferred + native`。
   - `codemode` → `enabled + hidden + codemode`。
   - `hidden` → `disabled + hidden + none`，保留“不可发现也不可调用”的旧语义。
   未知旧值仍使整份配置按现有策略备份并进入只读恢复；不做猜测或扩大调用通道。转换只作用于 `[mcp_servers].exposure.default` 和 per-tool overrides。
6. 设置页分别编辑可用性、目录行为和适配器，覆盖规则格式为 `pattern=availability,catalog,invocation`。旧 wire enum 与 server 级 Legacy/CodeMode 模式选项删除。

## 配置与数据影响

`config.toml` 中 exposure 字段从字符串 enum 改为含三个字段的对象。ConfigLoader 只转换已知旧值，并在成功加载后原子持久化规范形状；迁移失败保留原文件并使用现有备份/只读恢复行为。无需重置数据库、inbox 或凭据。MCP 连接协议、tool 参数和授权/日志持久格式不变。

## 替代方案

- 保留旧枚举并仅补 resolver helper：拒绝；同一个值仍会跨维度解释，UI 仍不能表达两个 adapter 同时可用。
- 把 `hidden` 只映射到目录隐藏：拒绝；旧 hidden 明确不可调用，转换必须同时禁用。
- 将 `model_only` 转成 `codemode` 或开放 Code Mode：拒绝；旧值没有 Code Mode adapter，不得扩大调用通道。
- 按 server 选择 Legacy/CodeMode：拒绝；这是 tool-level adapter 支持，且会使同一能力的调用契约再次受 server 配置耦合。

## 验收与限制

覆盖三维策略序列化、exact/wildcard override、native/deferred/codemode/both 路由、availability fail-closed、stale native/Code Mode 调用撤销和 legacy config 转换。跨端 DTO 由 Rust command contract 生成；Settings UI 对新对象作运行时 shape 校验。未改数据库 schema，源代码回滚即可恢复实现；已迁移配置可从迁移前备份恢复，或按[仅重建配置](../release-and-reset.md#仅重建配置)重建，无需清除用户数据。
