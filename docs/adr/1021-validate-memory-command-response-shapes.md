# 1021：从 Rust 契约校验完整 Memory 响应

## 状态

已采纳并实施（2026-10-12）。

## 背景

`list_facts`、`add_fact` 和 `recall_memory` 的 renderer parser 之前只检查部分实体 ID，然后把未知结果断言为生成 DTO。缺字段、错误字段类型或未知来源枚举仍能进入 Memory view；事实置信度、耐久度与提及次数也没有在 renderer 边界复核其领域值域。

## 决定

- IPC generator 从 App Rust Serialize DTO 生成 `MEMORY_FACT_RESPONSE_SHAPE` 和 `MEMORY_RECALL_ITEM_RESPONSE_SHAPE`；事实来源引用的嵌套字段随 DTO 递归生成。
- `memory.ts` 在命令边界使用通用 generated shape evaluator 校验全部必需字段、类型、nullable 与闭合 enum。未知附加字段继续开放，避免扩大 wire 契约并保持向前兼容。
- 领域校验继续归 Memory contract parser：事实及来源消息使用 canonical ID；`confidence` 和 `durability` 必须位于 `[0, 1]`；`mention_count` 必须是非负 JavaScript safe integer。Recall entity ID 按请求 kind 校验；score 必须是有限数，其他固定字段按 Rust DTO 类型检查。
- `check-ipc-contracts.ps1` 检查两份生成 shape 存在且 parser 实际调用它们。

## 影响

不改变 Rust DTO、数据库、IPC 字段或 Memory 查询语义。不完整或畸形响应会在 renderer boundary 失败；合法的附加响应字段仍保留。不存在数据迁移或重置要求。

## 替代方案

- 手写 TypeScript 对象 schema：拒绝，避免复制 Rust DTO 并引入漂移源。
- 只继续验证 ID 或要求拒绝所有未知字段：拒绝，前者不能保证消费完整性，后者会收紧无关的扩展兼容语义。

## 验证

- UI tests 覆盖有效完整行、附加字段、缺少字段、错误类型、未知 enum、数值边界、来源引用嵌套字段和 recall score。
- IPC generator/check、IPC ownership gate、Rust workspace check/test/clippy、UI check/test/build、ADR index 与 diff checks。

## 回滚

回退生成 shape 与对应 parser 检查会恢复旧的部分 ID 校验。实现不改变持久数据或 wire format。
