# ADR 0848：将工具参数 wire 转换归入 LLM adapter

## 状态

已完成（2026-10-10）。

## 背景

`haven_common::types::CanonicalToolCall` 是 Agent 与 LLM 共用的规范数据：调用 ID、规范工具名和已解析 JSON 参数。其实现同时公开了 `args_to_wire`、`from_wire_args`、`parse_wire_args`，并在 Common 保存 provider 参数 JSON 的空值分类、完成流截断修复算法；这些 API 的生产调用全部在 `haven-llm` 的 OpenAI Chat、OpenAI Responses 与 Anthropic adapters。`WireArgsParse` 也没有 Common 之外的消费者。`stream_tool_args_unfinished` 只有定义与单元测试，没有生产调用点。

该职责不满足 Common 的共享层准入：协议序列化和 provider stream 生命周期属于 `haven-llm`，Common 仅需提供跨 crate 稳定的规范数据类型。当前职责分散还让 canonical 类型承担了特定边界处理规则，并把无消费者的 stream helper 暴露为 Common 公共 API。

## 决定

1. 保留 `CanonicalToolCall { id, name, arguments }` 作为 Common 的规范数据类型；删除其 provider wire 方法和 Common 公共 `WireArgsParse` 类型。
2. 将 JSON 参数 wire 序列化、完成响应后的解析及结构截断修复移至 `haven-llm::adapters::tool_arguments` 私有模块。OpenAI Chat、OpenAI Responses、Anthropic 只通过该模块转换 provider 载荷。
3. 删除无生产消费者的 `stream_tool_args_unfinished`，不保留 alias 或兼容入口。
4. 移动解析/修复单元测试到 LLM 私有模块。算法、空白参数映射 `{}`、合法 JSON 保真、结构性截断修复、字符串中断与不可修复输入映射 `null` 均保持现状。

## 替代方案

- 继续把转换方法留在 `CanonicalToolCall`：拒绝。所有生产调用仅属于 LLM adapters，保留会让共享数据类型继续拥有协议边界职责。
- 每个 provider adapter 分别实现解析/修复：拒绝。调用点虽跨多个 provider，但参数 JSON 规则相同；复制会产生多个算法 owner。
- 为 Common 保留公开分类或兼容 alias：拒绝。仓库内没有 Common 外分类消费者，且项目明确不要求向下兼容。

## 影响与验证

Common 公共 API 删除 wire 参数方法和解析分类类型；workspace 内消费者统一改为 LLM adapter 私有 helper。跨层规范数据仍为 JSON `Value`，外部 provider wire 输出和完成流解析行为保持不变。无 IPC、配置、SQLite 或用户数据变化，无需重置数据。

验证范围：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`，以及旧入口全仓搜索和 `git diff --check`。

## 回滚

若 adapter 级解析出现回归，将 `tool_arguments.rs` 的实现与测试移回 Common 并恢复旧方法调用；无持久数据回滚或重置步骤。
