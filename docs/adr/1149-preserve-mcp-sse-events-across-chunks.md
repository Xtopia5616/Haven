# 1149：跨 chunk 保留完整 MCP SSE 事件

## 状态

已采纳并实施。

## 背景

SSE parser 在每次 `feed` 局部保存 `data:` 行集合；多行 JSON event 若跨 chunk 到达会丢失先前行。另一个 chunk 即使包含多个完整小 event，只要 chunk 总长度超过 cap，也会被整体丢弃，可能使 JSON-RPC response 等待至超时。

## 决定

- parser 将未完成行和当前 event data state 跨 `feed` 调用持有。
- 按行增量消费 chunk，使总 chunk 大小不触发整块丢弃；每条 incomplete line 与累积 event data 都受同一 byte cap 限制。
- 超限行丢弃至 newline；超限 event 丢弃至空行分隔符，随后恢复解析后续 event。

## 影响

多行 event 跨任意网络 chunk 边界仍被正确组装；含多条完整事件的大 chunk 可以正常解析。单条超限行/event 仍 fail closed，解析内存保持有界。

## 验证

新增跨 feed 多行事件、单个大 chunk 内多条小事件以及超限后按分隔符恢复的 parser 回归。运行 MCP/workspace 测试与严格 Clippy。

## 回滚

无持久数据重置要求。不得恢复每 feed 重置 data 行或以 chunk 总长替代 incomplete line/event cap 的判断。
