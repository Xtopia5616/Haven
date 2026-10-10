# ADR 0860：集中 UI 运行时值校验原子谓词

## 状态

Accepted — 2026-10-10

## 背景

多个 UI 运行时边界各自维护相同的基础判断：Agent、App、Session、ToolRun mapper 重复声明 `WireRecord`、字符串字段读取、有限数、可选字符串和枚举 membership；MCP status、Memory、录音、媒体、Session 状态、ToolRun、工具 manifest、主题偏好、工具结果 renderer 与 interaction reducer 又各自实现闭合字符串集合 membership 或基础值类型检查。集合成员判断散布在 `includes`、`some` 和带类型断言的变体中，通用谓词没有唯一 owner。

这些 mapper 的 field policy 并不相同：必填和可选字段、缺省与显式 `null`、空字符串和未知扩展字段有不同的 wire 约定。因此只能合并原子类型判断，不能合并各领域的完整 payload validator。

## 决定

- 在 `ui/src/lib/contracts/valueGuards.ts` 唯一拥有纯值谓词：`isString`、`isBoolean`、`isNonEmptyString`、`isNumber`、`isFiniteNumber`、`isStringArray` 和 `isOneOf(value, values)`。`isNumber` 保留所有 JS number；需要有限值时调用者使用 `isFiniteNumber`。
- 在 `ui/src/lib/contracts/wireGuards.ts` 唯一拥有 wire-record 字段操作：`WireRecord`、`hasOwnWireField`、`readStringField`、`nonEmptyStringField` 与 `optionalStringFieldIsValid`。字符串读取和非空读取分名；后者不承担实体 ID 前缀/UUID 格式校验。
- Agent、App、Session、ToolRun、MCP status、Memory、录音、settings response、media、manifest、theme、工具结果 renderer 与 interaction reducer 复用上述谓词；保留各自命名的领域 guard 作为有类型意义的入口。
- `toolResultValidation.ts` 的 JSON family shape validator 仍由该领域模块拥有；本轮只把纯值谓词迁入共享 guard。保留该文件中独立的未提交 schema 扩展 hunks，未改动其 shape policy。
- 必填、缺省/null、字段范围、动态 shape、fail-closed/默认值及错误处理继续归领域 mapper。`isOneOf` 只核对给定 closed string tuple 的成员关系，不创建全局 enum registry。
- 不改变 wire DTO、持久化格式、错误/默认语义或运行时接受集合。

## 替代方案

- 在每个 mapper 保留相同的原子判断：拒绝。重复集合 membership 和基础类型谓词会漂移，也造成 `includes` 参数断言与 `some` 实现并存。
- 建立统一的全局 payload schema/validator 并取代领域 mapper：拒绝。不同事件、动态 Tool result 与本地偏好有不同 field policy；全局 schema 会合并不等价的缺省、null 与扩展语义。
- 把实体 ID 格式校验塞进 `nonEmptyStringField`：拒绝。该 helper 只保证值非空；规范 ID 前缀、外部 provider ID 与进程内 ID 的边界另行审计并由相应规则拥有。

## 影响与验证

此切片只归并 UI 运行时验证的原子 guard，字段策略仍在调用方。没有 IPC payload、事件顺序、配置、SQLite schema 或用户可见错误行为变化。现有 mapper 的测试保持原样；按执行约束未运行测试套件。

验证通过：`corepack pnpm exec prettier --write`（本 ADR 涉及源文件）、`corepack pnpm run check`、`corepack pnpm run build` 与 `git diff --check`。

## 回滚

若某个边界需要不同的基础值语义，应为该差异命名独立的领域谓词并说明反例；通用 guard 保持共享。回滚时恢复对应 mapper 的内联 guard、删除 `valueGuards.ts` / `wireGuards.ts` 与本 ADR 决定，不保留兼容别名。无持久数据需要重置。
