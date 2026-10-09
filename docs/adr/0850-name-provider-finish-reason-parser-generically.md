# ADR 0850：移除结束原因解析器的 OpenAI 专属命名

## 状态

已完成（2026-10-10）。

## 背景

`FinishReason::from_openai()` 接受 `stop`、`end_turn`、`tool_use`、`max_tokens`、`safety` 等多个协议的标签，并由 OpenAI、Anthropic、Gemini adapters 共用。Gemini adapter 先处理其专有大写枚举，再对其它共享标签转小写后调用该方法。方法名和注释把跨 provider 的 canonical parser 误称为 OpenAI 专有入口。

结束原因的规范枚举是 provider-neutral；未知或 provider 专有的值仍由对应 adapter 决定如何映射，不应通过扩充一个带错误 provider 名的公共解析方法表达。

## 决定

1. 将 `FinishReason::from_openai` 重命名为 `FinishReason::parse_provider_value`，明确其输入是已规范化大小写的 provider 标签，输出是共享结束分类。
2. OpenAI、Anthropic 与 Gemini adapters 全部改用该名称；Gemini 的专有值映射、大小写归一化位置和 fallback 顺序保持不变。
3. 测试以跨 provider 共享标签（`end_turn`、`tool_use`、`max_tokens`、`safety`）证明解析器真实范围，并保留未知/未归一化标签返回 `None` 的断言。

## 替代方案

- 将 parser 移入 OpenAI adapter：拒绝。Anthropic 与 Gemini 也真实调用相同映射，迁移会反转 owner 边界或增加重复实现。
- 保留 `from_openai` 并只改注释：拒绝。调用点仍会把非 OpenAI 协议误写成 OpenAI 转换。
- 给不同 adapter 复制映射：拒绝。相同的共享标签应只有一个 canonical parser；协议专有枚举继续留在各 adapter。

## 影响与验证

这是 `haven-llm` crate 内部解析器重命名；返回值、大小写要求、别名集合与未知值行为不变。无 IPC、配置或持久化变化，无需重置。验证：`cargo fmt --all -- --check`、`cargo test --locked -p haven-llm`、`cargo clippy --locked -p haven-llm -- -D warnings`、旧名全仓搜索、ADR 索引/链接检查及 `git diff --check`。

## 回滚

恢复原方法名及 adapters 的调用即可；无需数据或配置回滚。
