# 1011：校验 builtin manifest 列表 envelope

## 状态

已采纳并实施（2026-10-12）。

## 背景

`list_builtin_tool_manifests` 的 Rust 响应使用 `{ tools: [...] }` envelope，但 UI command adapter 直接把 generated 静态类型当作运行时校验。错误对象或缺失/非数组 `tools` 会被 `ToolsView` 当成空目录；逐项 manifest parser 只有在 envelope 已成功返回后才运行。

## 决定

- `listBuiltinToolManifests` 在命令边界验证响应是对象且 `tools` 是数组，并保留 additive envelope 字段。
- 单个 manifest 继续交给 `setToolManifests` / `parseToolManifest` 验证，不在命令 adapter 复制 manifest schema。
- 非法 envelope 作为 command failure；错误 manifest 行继续由目录 owner 排除。

## 影响

Rust DTO、JSON shape、动态 `input_schema` 与 manifest view 不变。畸形 envelope 不再默认为有效空目录；无持久化或重置要求。

## 替代方案

- 只依赖 generated TypeScript 类型：拒绝；类型不会在 IPC runtime 验证 payload。
- 在 command adapter 重复解析所有 manifest 字段：拒绝；`toolManifest.ts` 已是逐行 parser owner。

## 验证

- `corepack pnpm exec vitest run src/lib/toolsCommands.test.ts`
- `corepack pnpm run check`

## 回滚

恢复直接返回 generated response 即可回滚；错误 envelope 将再次被消费为缺省/空工具列表。
