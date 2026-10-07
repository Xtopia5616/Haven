# ADR 0698：共用 untrusted record guard

## 状态

已采纳并实施。

## 背景

Agent、App、Session、Settings、ToolRun、MCP status、Tools command 与 tool result parser 都各自实现了相同的对象判断：值必须是非空对象并且不是数组，随后按 `Record<string, unknown>` 读取。`toolResultParsing.ts` 将同一判断命名为 `isObject`。这些实现的输入、拒绝条件与类型收窄一致，没有领域专属语义。

## 决定

- 在 `contracts/objectGuards.ts` 提供唯一 `isRecord` 类型守卫。
- 所有 IPC、命令响应与动态工具结果边界共用此 guard；移除局部同义实现，将 `toolResultParsing` 的 `isObject` 改为 `isRecord`。
- 保留各 contract 中 `WireRecord` / `SessionWireRecord` 等局部用途名，以及其余字段校验；本决定只统一底层对象识别。

## 替代方案

- 每个边界继续保留同样的 predicate：拒绝。若接受条件需要改变，重复实现会产生不一致。
- 把 `isRecord` 与其它字符串、数字、日期校验合成通用 validator framework：拒绝。其它判断随 payload 领域而异，没有相同 owner 证据。

## 影响与验证

- 对象、数组、null 与 primitive 的接受/拒绝行为不变；不影响 IPC、持久数据或配置，无需重置。
- 验证：新增 guard 单元测试，运行 UI `check`、`test:run`、ADR 索引及 `git diff --check`。

## 回滚

将共享 import 替换回各模块内的原 predicate 即可；无数据或 wire 迁移。
