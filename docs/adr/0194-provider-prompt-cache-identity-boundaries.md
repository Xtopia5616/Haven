# ADR 0194：Provider prompt cache identity boundaries

- 状态：Accepted
- 日期：2026-09-21

## 背景

ReAct context compaction keeps the early conversation anchor but replaces the
middle with a summary. The provider-visible prefix after that anchor is
therefore a new cacheable prefix. The same request surface can also change
when tool/Skill/MCP definitions, web-search mode, or media representation
capabilities change.

## 决定

1. Compaction replaces the in-memory canonical generation and the OpenAI
   routing key includes a stable marker for the latest compaction root. The
   key stays stable for later turns until another compaction occurs.
2. OpenAI Chat and Responses routing keys include the adapter capability
   profile, the complete provider tool projection, and web-search mode.
3. The key includes the raw media surface (kind and MIME metadata), but never
   media bytes. Raw-media and text-fallback projections use different cache
   identities without creating a new shard for every attachment's contents.
4. The system prompt's stable section remains the cross-session cache anchor;
   volatile MEMORY and session sections remain excluded from the routing key.

## 影响与验证

The cache identity is process/provider-request metadata only. No cache key or
prompt content is persisted. Regression tests cover compaction boundaries,
post-compaction stability, tool schema projection, web-search mode, and raw
media surface behavior.

验证命令：

```text
cargo test --locked -p haven-llm --lib prompt_cache_key
cargo test --locked -p haven-agent --lib react::state::tests
```

## 回滚

Revert this ADR and the associated code/test changes. No database, snapshot,
configuration, or user-data reset is required.
