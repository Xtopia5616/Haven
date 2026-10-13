# 1152：在 Tools 组合层适配 LLM 的 MCP caller

## 状态

已接受并实现（2026-10-13）。

## 背景

`haven-mcp` 直接依赖 `haven-llm`，唯一用途是让 `McpManager` 实现 `haven_llm::McpToolCaller`，供 LLM STT 的 MCP provider 调用。MCP manager 因而依赖一个由上层消费方定义的接口，违反单向依赖。`haven-tools` 已同时依赖两个领域 crate，并在 `ToolServices` 装配 MCP 服务，是连接两者的组合位置。

## 决定

- 删除 `haven-mcp → haven-llm` 依赖，并删除 manager 上的 LLM trait 实现；删除随实现一并不再使用的 `async-trait` 直接依赖。
- 在 `haven-tools` 内新增私有 adapter，持有 `McpManager` 并实现 `haven_llm::McpToolCaller`。适配器仅在 `ToolServices` 组装消费接口时构造；不向 App 暴露具体 manager。
- 逐字段映射 `McpCallOutput` 到 `McpToolOutcome`，保持原有工具调用、取消和错误传播行为。

## 影响与数据

Rust crate 依赖图收敛一条边。MCP/STT 运行时行为、配置、provider 请求、IPC、数据库和持久化数据不变；无需重置或迁移。

## 替代方案

- 保留由 `McpManager` 实现 LLM 接口：拒绝，因为会让通用 MCP crate 依赖上层消费方。
- 在 App 组合根实现 wrapper：拒绝，因为 manager 由 Tools 持有，向 App 传递调用能力需新增并暴露另一项跨 crate port；Tools facade 已经负责装配 MCP 与 LLM 消费接口。

## 验证

通过：`cargo fmt -p haven-mcp -p haven-tools -- --check`、`cargo check --locked -p haven-mcp`、`cargo test --locked -p haven-mcp`（74 项）、`cargo clippy --locked -p haven-mcp -- -D warnings`、`scripts/check-crate-dependencies.ps1`、`scripts/check-adr-index.ps1` 与 `git diff --check`。Tools adapter 模块本身由临时隔离 harness 编译并运行了 2 项测试（字段映射、manager 错误传播）。

未完成 workspace 验收：`cargo check --workspace` 在既有未提交的 `crates/skills/src/venv.rs` 因 `python` 被 async closure 移动后继续使用而失败；因此 haven-tools crate 全量测试及 workspace test/clippy 未能编译执行。`cargo fmt --all -- --check` 也报告既有未提交的 `app-binary` 与 `memory` 文件格式差异。本次保留这些工作区改动未触碰。
