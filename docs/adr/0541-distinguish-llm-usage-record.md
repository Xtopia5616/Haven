# ADR 0541：区分运行时 LLM 用量与持久用量记录

## 状态

已采纳并实施；Rust workspace、IPC 与 UI 门禁通过。

## 背景

`haven_llm::LlmCallUsage` 是一次 provider 调用完成后，由 LLM/Tools 返回给上层的运行时元数据，仅含请求种类、provider usage、模型和耗时。`haven_memory::repositories::usage::LlmCallUsage` 则是持久化的一次调用明细，另外包含 `usage-*` ID、session/step、调用 owner、成本、context 统计和创建时间；它被 `SessionStore` 作为 `usage_recorded` event payload 保存，并进入 `llm_usage` 投影，也由 resume command 输出并生成前端 TypeScript contract。相同名字遮住了数据形状、生命周期和 owner 差异。

## 决定

1. 将 Memory 持久明细类型改名为 `LlmUsageRecord`，其写入输入改名为 `LlmUsageRecordInput`；LLM runtime `LlmCallUsage` 保持原名。
2. 更新 Agent、App、Memory、当前架构输出清单和 UI contract 引用；重新生成 `generatedCommands.ts`，使静态类型名与持久记录职责一致。
3. 保持 `llm_usage` 表、`usage_recorded` payload 字段、Tauri JSON key/shape 及运行时累计和 rollback 行为不变。Serde payload 不包含 Rust 类型名，因此既有 durable event 无需迁移。

## 替代方案

- 合并成一个类型：拒绝。provider 返回值缺少 durable identity、session owner 和持久投影字段；让 LLM 层承担 Memory 持久职责会跨越 crate 边界。
- 保留两个同名类型：拒绝。模块路径虽可在 Rust 引用中消歧，生成的 IPC DTO 与调用点仍容易把 runtime metadata 和 durable record 混为一谈。
- 重命名 LLM runtime 类型：拒绝。它已准确表达一次调用产生的运行时 usage 元数据，也被 Tools/Agent 作为无 session 信息的输入使用；改 Memory 类型更能体现真实责任。

## 影响与验证

- Rust 公共类型名及生成 TypeScript interface 名变化；运行 JSON/IPC 字段、SQLite schema、事件 payload 与历史记录不变。无需数据重置或迁移。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`scripts/check-ipc-contracts.ps1`（81 handlers）、`corepack pnpm run check`（0 errors / 0 warnings）、`corepack pnpm run test:run`（122 files / 974 tests）、`corepack pnpm run build`、`scripts/check-adr-index.ps1`（524 条唯一编号记录）与 `git diff --check`。Rust 手动性能 profile 按约定 ignored；Vitest 输出既有 Node `TimeoutNaNWarning`，测试通过且退出码为 0。

## 回滚

恢复 Memory `LlmCallUsage`/`LlmCallUsageInput` 命名和对应生成的 TypeScript interface 引用，再运行 IPC generator；字段 shape 和持久数据无需回滚。
