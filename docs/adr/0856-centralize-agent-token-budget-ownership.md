# ADR 0856：集中 Agent token budget owner

## 状态

Accepted — 2026-10-10

## 背景

`prompt.rs`、`prompt_renderer.rs` 与 `compactor.rs` 各有一份 `truncate_to_token_budget` 前缀截断二分算法。差异只在零预算分支的写法；三者使用同一个 `estimate_tokens`，但该 tokenizer、惰性初始化和失败回退原先放在 `compactor.rs`。ReAct、prompt builder 和纯 renderer 因此都从 compactor 模块取得 token accounting，导致职责入口名与实际消费者不符。

Canonical message/tool/request 的 provider-visible token estimate 也定义在 compactor 模块中，并被 ReAct request preflight、增量 message cache 和 prompt 渲染共同使用。它们没有持久状态；权威规则是同一 estimator、同一 provider request overhead 和同一 tokenizer fallback。

## 决定

- 在 Agent 私有模块 `token_budget.rs` 中集中 tokenizer 初始化与错误回退、canonical message/tool/request estimate、provider request overhead，以及前缀和后缀 token budget 截断。
- 移除三份重复的前缀截断实现，调用方使用 `truncate_prefix_to_token_budget`；后缀保留操作命名为 `truncate_suffix_to_token_budget`，明确输出方向。
- ReAct、prompt builder、renderer 与 compactor 均从 token budget owner 读取 token estimate。`ReActState` 仍负责其版本作用域增量 estimate cache；它不复制 tokenization 规则。
- `compactor.rs` 继续拥有 compaction 范围规划、prefix sums、消息裁剪和 summary 流程；`prompt_renderer.rs` 继续拥有完整行选择和 MEMORY fence 布局。
- 删除没有消费者的 `estimate_provider_request_tokens_with_message_estimate` 中间 wrapper；保留完整 estimate 与缓存 estimates 两个实际使用入口。

## 替代方案

- 保留三个截断实现并只同步修补：拒绝。零预算分支已经出现写法分歧，同一最大前缀定义应只有一个实现。
- 让 compactor 继续拥有所有 estimate API：拒绝。ReAct 和 prompt 渲染并不消费 compactor 生命周期，模块名会继续掩盖 token accounting owner。
- 把 token 估算移到 Common：拒绝。o200k tokenizer、provider-visible canonical 计价及其 fallback 属于 Agent 的 prompt/request 策略，不是跨 crate 的稳定纯共享契约。
- 把 compaction 的范围规划和按行渲染也全部移入 token budget：拒绝。这些算法保留/选择不同消息与结构边界，不是通用文本 token-fit 操作。

## 影响与验证

这是 `haven-agent` crate 内部函数归属与调用路径调整；没有改变 tokenizer、fallback、estimate 字段、预算常量、裁剪方向或结果，没有 IPC、配置和持久化变化，无需数据重置。既有估算、边界与 compaction 行为测试保持原断言；本轮按执行约束未运行测试套件。

验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-agent`、
`cargo clippy --locked -p haven-agent -- -D warnings` 与 `git diff --check`。未运行测试套件。

## 回滚

若实际消费者证明 token accounting 与 prompt budget 操作并非同一 owner，可整体恢复 estimator/truncation 到原模块并恢复原调用路径；不保留 compactor 命名兼容别名。无持久数据需要重置。
