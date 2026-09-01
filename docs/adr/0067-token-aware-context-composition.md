# ADR 0067：Token-aware 上下文拼接与压缩

## 背景

上一轮缓存重构已经解决了派生结果缓存的容量和失效一致性，但 ReAct
上下文本身仍有两个热点：Additional context 中的一条长历史消息可能挤掉
所有其他近期消息；compaction 按消息数量选择中间区域，无法反映工具结果、
思考内容和多模态内容的真实 token 成本。压缩请求也可能因为被压缩区域过大
而超过摘要模型的上下文窗口。

## 决定

- Additional context 保留既有的 newest-first 预算打包和确定性顺序，同时将
  单条历史项限制为 1200 字符，避免单条异常输出垄断整个上下文预算。
- ContextCompactor 为每条 canonical message 计算一次 provider-visible 成本，
  使用前缀和选择压缩边界。系统消息和最早两个非系统消息作为 sticky prefix
  保留，最近尾部按 token 而不是消息数尽可能保留，压缩后的目标约为触发阈值
  的三分之二。
- 压缩范围仍只能跨完整工具轮次；assistant tool call 与其 tool results 不被
  拆开，防止 provider 拒绝上下文。
- 摘要输入上限为 16k token；超出时保留被压缩区域的首尾并插入省略标记。
  摘要输出上限为 768 token，防止压缩后的新消息重新占满窗口。
- 摘要输入包含工具调用名称、参数和 tool-call id 的结构化行，避免压缩后
  只剩自然语言而丢失关键执行上下文。

## 替代方案

- 继续按消息数量取一半：实现简单，但一条大型 observation 会让 token
  分布严重失真，拒绝。
- 只截断最近历史：能降低 token，但会丢失早期用户意图与工具结果，拒绝。
- 将整个压缩区原样送给摘要模型：短会话无差别，长会话可能令摘要请求自身
  超窗，拒绝。
- 每次 compaction 都重写系统提示和整个 canonical：会扩大 provider prompt
  cache 的失效前缀，拒绝。

## 影响与验证

这是 Agent 进程内上下文规划重构，不改变 snapshot、events、数据库 schema、
IPC 或 provider wire 契约。压缩会保留稳定前缀和近期尾部；摘要输入/输出和
Additional context 均有明确边界。缓存与上下文的锁、取消和持久化语义不变。

重点测试包括：token-aware 最近尾部选择、完整工具轮次边界、16k 摘要输入上限、
摘要输出上限，以及单条超长历史项不会挤掉其他近期项。

```text
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

## 回滚

回退本 ADR 对应提交即可恢复旧的消息数量型 compaction 和历史拼接策略；本变更
没有数据库或用户数据迁移，重启进程即可清理进程内派生状态。
