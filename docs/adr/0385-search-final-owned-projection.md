# ADR 0385：search-final 消息投影归属 owning commit

- 状态：已采纳并实现（2026-09-28）
- 关联：[ADR 0336](0336-react-session-committed-submission.md)、[ADR 0361](0361-final-architecture-acceptance-audit.md)

## 背景

server-side search 与合成 final answer 同时返回时，ReAct 先提交携带 `web_search_calls` 的 ToolCall event。若响应没有 Thought 文本，turn-end 再通过 `ProjectChatMessage` 单独写入合成 final，导致可恢复内容的消息投影不属于产生搜索上下文的 commit intent。普通无 Thought final 已由其 ToolCall intent 同时写 events 与 messages；只有 search-final 分支保留了这条旁路。

## 决定

1. 对同一响应包含 server-side search 与合成 final 的路径，在 `prepare_search_context` 构造 ToolCall intent 时确定消息投影文本和 `thought` message id。已有 Thought 时复用它；无 Thought 时复用 turn-end 既有的 `Session completed.` 文本和该 step/run 的稳定 thought id。
2. `finish_turn_end` 在 final transcript 已由前置 ToolCall commit 拥有时只负责注入队列和暂停边界，不再另发 `ProjectChatMessage`。
3. search-only round 仍只提交搜索上下文、保存 branch point 并继续下一轮；不把 `response.text` 转成最终消息。ask/waiting notice 仍是明确的 UI-only 直接投影例外。
4. `ProjectChatMessage` 与 `persist_session_message` 的文档移除 search-final 例外；未来可恢复 transcript 文本仍必须通过 `SessionCommitted`。

## 影响与验证

events、canonical 内容顺序、final 文本、message ID、搜索数据、UI sequence 和 pause 边界保持不变。唯一变化是 search-final 的 ToolCall event 与其 message projection 现在由同一个事务提交，物化失败时两者一起回滚。

```text
cargo test --locked -p haven-agent synthesized_search_final_is_projected_by_its_tool_call_commit --lib
cargo test --locked -p haven-agent
cargo check --workspace --locked
```

## 回滚

恢复 `finish_turn_end` 中的 search-final `ProjectChatMessage` 分支，并还原 transcript 文档的例外说明即可；不涉及 schema 或数据库重置。
