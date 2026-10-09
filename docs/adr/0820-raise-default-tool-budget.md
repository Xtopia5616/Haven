# ADR 0820：将默认工具请求预算提高到 256

## 状态

已接受（2026-10-09）

## 背景

`context_limits.max_tools_per_request` 同时约束 provider 单次请求中的工具定义数量，以及 builtin、Skill、MCP 按需加载到当前 session 的准入预算。默认值 64 会使内置能力目录中大量 operation 无法在同一 session 中加载。

## 决定

将默认值从 64 提高到 256，并同步 Rust 配置默认值、UI 缺省值、设置说明和默认配置测试。该字段仍可由用户配置；已有显式值继续生效。

该预算仍是单次 provider 请求的总工具数上限，builtin、Skill 和 MCP 加载仍由同一预算原子准入。256 低于既有文档记录的 provider 约 350 项硬上限，并为 provider 差异留出空间。

## 替代方案

- 保持默认 64：拒绝。内置 catalog 持续增长后，该值过度限制模型按需加载 builtin operation。
- 移除预算或默认设为 350 及以上：拒绝。provider 之间的硬限制不同，仍需保留统一的保护余量。

## 影响与验证

只改变缺省配置，不改变配置字段、schema、持久化格式或数据库；没有显式设置该字段的配置在加载时采用 256，显式设置的用户值不变，无需迁移或重置。单次请求最多可包含更多工具定义，token 消耗可能随实际加载数量增加。

适用门禁：配置默认值测试、`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、UI check/test/build，以及 `git diff --check`。

回退时将 Rust 与 UI 默认值恢复为 64，并恢复设置提示即可；已有配置不需要变更。
