# ADR 0077：Agent 可刷新上下文的缓存前缀布局

## 背景

Agent 每次 ReAct 回合都会重新发送 canonical 对话。静态指令、当前会话上下文
和跨会话 MEMORY 原先都位于对话历史之前；事实抽取写入后刷新 MEMORY，会使
后续完整历史从 provider prompt cache 的可复用前缀中被截断，导致缓存率下降。

## 决定

1. 继续以 `SESSION_CONTEXT_FENCE_START` 划分静态指令和动态上下文，并进一步
   区分本次运行内稳定的会话上下文与可刷新 MEMORY。
2. OpenAI Responses 保留静态 `instructions` 和前置 developer 会话上下文；将
   MEMORY 放到 input 尾部的 provider-only developer item，保持 developer 优先级。
3. OpenAI Chat 保留前置 system 指令和会话上下文；将 MEMORY 放到 input 尾部的
   provider-only user item。Chat 兼容层要求 system 消息位于前部，MEMORY 已用
   fence 标记为引用数据，不作为新用户指令处理。
4. Responses 的 legacy developer-role 降级一次性合并全部 provider-only item，
   不因拆分后的多个 item 消耗额外 retry budget。
5. OpenAI 两种 wire 的工具 schema 递归按 JSON object key 排序；缓存 routing key
   使用实际 provider tool projection，并将 Responses 内置 web-search 模式纳入
   key，避免等价 schema 或不同工具表面误选缓存分片。

## 替代方案

- 继续把 MEMORY 放在对话历史前：实现最简单，但 MEMORY 刷新会让整个历史前缀
  失去复用。
- 把所有动态上下文都移到请求末尾：会丢失本次 ReAct 运行中稳定会话上下文的
  developer/system 优先级，并削弱跨回合增量缓存。
- 忽略工具 schema / web-search 模式：routing key 可能与实际 wire 前缀不一致，
  会污染缓存分片，不能接受。

## 影响与验证

变更只作用于 provider 请求 projection，不写入 events、canonical transcript、
snapshot、数据库或 IPC。新增 JSON canonicalization 无容量状态；provider-only
上下文继承请求取消和现有串行 ReAct 回合生命周期。回归测试覆盖 MEMORY 刷新时
Chat/Responses 对话前缀保持、工具 schema cache identity、web-search key 以及
legacy developer-role 降级。

验证命令：

```text
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

## 回滚

回退本 ADR 对应提交即可恢复旧 provider projection；不涉及数据库、配置或用户
数据重置。
