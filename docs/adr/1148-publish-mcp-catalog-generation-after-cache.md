# 1148：在发布工具缓存后再提升 MCP catalog generation

## 状态

已采纳并实施。

## 背景

MCP `tools/list_changed` listener 原先先通知 manager 提升 catalog generation，再 await `tools/list` 并替换 `tools_cache`。Agent prompt schema cache 以该 generation 为 key；并发 prompt 可能在 cache 仍旧时把旧工具定义缓存到新 generation，而刷新完成后没有第二次失效。

## 决定

- 收到通知后先完成 tools/list 请求并发布成功的新 cache，或在失败时清空 cache。
- 只有 cache 更新结束后才通知 manager 提升 generation；新 generation 始终代表已发布的 cache 状态。
- prompt 在 refresh 期间完成的旧 generation build 不会在后续 generation 中命中；失败清空 cache 也会触发同样失效。

## 影响

并发 prompt 不会把旧工具 schema 标成刷新后的 generation。MCP server/tool schema 仍由 MCP manager/client owner 管理，不新增第二套 cache version。

## 验证

新增回归在 catalog invalidation callback 中读取 cache，断言 cache 已包含刷新结果。运行 MCP、Agent 与 workspace 测试和严格 Clippy。

## 回滚

无持久数据重置要求。不得恢复“先递增版本、后替换工具缓存”的顺序。
