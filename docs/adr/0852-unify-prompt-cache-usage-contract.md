# ADR 0852：统一 Prompt Cache 用量契约

## 状态

Accepted — 2026-10-10

## 背景

Prompt Cache 用量信息跨越 provider adapter、`Usage`、Agent 累计/写入路径、`agent:usage` 事件、Memory 的 `llm_usage` 记录和 UI。此前 `CacheDiagnostics` 定义在 `haven-llm`，其 `mode`、`outcome`、`usage_source` 是附带固定词汇说明的开放 `String`；Agent 为 Memory 写入再序列化一次，Memory 的输入使用 JSON 字符串、读取记录使用 `serde_json::Value`，UI 事件 mapper 仅把 diagnostics 当作未验证 `unknown`。同一对象因此有多份运行时形状，类型约束在 LLM 返回后消失。

同一用量域中的 `CacheAccounting` 和 `LlmCallKind` 已是 Common 闭合枚举，但位于通用 `types` 模块；Memory 暴露的 `LlmUsageRecord.cache_accounting` 又退化为 `String`。另外，diagnostic 字段 `mode` 与 UI 已使用的“缓存策略”概念不一致，无法准确说明它表示实际采用的 request strategy。

## 决定

- 在 `haven-common::usage` 设立跨层用量值契约 owner，集中定义 `CacheAccounting`、`LlmCallKind`、`CacheDiagnostics` 及 `PromptCacheStrategy`、`CacheDiagnosticOutcome`、`CacheUsageSource` 闭合枚举。旧 `haven_common::types` 和 `haven_llm` 类型入口删除，不保留兼容别名。
- `CacheDiagnostics.mode` 改名为 `strategy`；三个词汇字段都使用枚举。`provider` 保留开放字符串，因为它标识用户配置的 provider 名称；诊断对象仍不包含 prompt 或 cache key。
- `haven-llm` 只负责从 provider request/response 生成该契约；Agent 的 `UsageUpdate`、事件 payload 和 Memory 输入/输出直接使用 Common 类型，不在 Agent 与 Memory 间预先 JSON 编码。
- App event 和生成的 TypeScript DTO 使用同一 Rust 值类型。UI event mapper 校验 `strategy`、`outcome`、`usage_source` 及其它必需字段；用量展示直接消费该契约，不再定义动态诊断摘要副本。
- `LlmUsageRecord.cache_accounting` 与 `cache_diagnostics` 改为闭合类型。SQLite `cache_accounting TEXT` 与 `cache_diagnostics TEXT` 仅在 Memory repository 内编码/解码；typed JSON 诊断只在该持久化边界序列化。
- 保持 token 计算、provider 归一化、缓存命中判断、费用与累计值语义不变。旧诊断 JSON 的 `mode` 字段不会作为 `strategy` 别名读取；无法解析的历史诊断被忽略为缺省诊断，不影响该用量行、token 总数、回滚或恢复。

## 替代方案

- 只把 CacheDiagnostics 移进 Common，继续保留开放字符串及 Agent→Memory JSON 字符串：拒绝。它仍允许无效值跨过关键边界，并保留重复编码层。
- 保持 `LlmUsageRecord` 的 `Value`/`String` 形状：拒绝。诊断的字段集和值域在生产代码中已经固定，动态 JSON 没有协议扩展用途。
- 将 `provider` 也变成 enum：拒绝。用户可配置自定义及兼容 provider 名称，不属于 Haven 可闭合的 protocol vocabulary。
- 只在 UI 把 `mode` 显示为“策略”：拒绝。内部字段和持久诊断仍会用宽泛字段名表达另一概念。

## 影响与验证

这是 Rust 公共类型、`agent:usage` IPC/event、TypeScript mapper、Memory DTO 和缓存诊断 JSON 字段名的破坏性收敛。不改 SQLite 表结构、列类型、token 数值口径或 ID；无需 schema migration 或数据库重置。旧诊断 JSON 可能不再显示，但它不是恢复/计费权威数据。

本切片已通过 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、
`corepack pnpm run build`、变更的手写 UI source/test 文件 Prettier 检查、IPC command/event contract 检查
（生成的 `generatedCommands.ts` 由生成器与 contract 检查验证）、
crate dependency 检查、ADR index 检查与 `git diff --check`。这些结果只覆盖本次契约收敛；
不会据此宣称全项目命名审查完成，路线图 §5.7 仍 Active。

## 回滚

若需回退，须整体恢复 Common 用量契约、provider adapters、Agent/Memory DTO、事件/生成 TypeScript contract、UI mapper 与文档；不得只为旧 `mode` 或旧 dynamic JSON 增加双读兼容路径。
