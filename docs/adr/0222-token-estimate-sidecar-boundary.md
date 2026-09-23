# ADR 0222：Token estimate 有界进程内缓存归属

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 的 token estimate sidecar 与 SessionActor mailbox
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)

## 背景

`TokenEstimateCache` 只缓存 canonical transcript 的 token 估算，用于加速后续上下文预算判断。canonical transcript 才是事实来源；缓存有固定容量上限，逐出或进程重启只会导致重新估算，不影响会话恢复。当前缓存放在 `SessionRuntimeState`，估算、追加和清理都经 SessionActor mailbox，产生不必要的命令与 canonical 消息复制。

## 决定

1. `TokenEstimateCache` 是进程内、有界、不可持久化的纯缓存，不属于 durable state，也不需要由 SessionActor mailbox 管理。
2. `ReActEngine` 直接拥有并初始化该缓存；canonical estimate、append delta 与 session cache reset 均由 engine 直接调用现有 `TokenEstimateCache` API。
3. 保持 generation、revision、append 长度校验及容量淘汰语义不变；不改 `sidecars.rs` 的实现和测试。
4. 这是 ADR 0214 的局部切片：仅删除 token estimate 的 actor runtime 字段、命令、handle 方法和 match arms。Usage / `UsageTracker` mailbox 留待后续切片处理。
5. 不改变 canonical/transcript/compaction、数据库、UI、schema、wire 或 token 算法。

## 替代方案

- 继续经 actor mailbox 调用：为纯进程内缓存保留异步命令与 canonical 复制，没有持久化或 actor 协调收益。
- 把缓存放入 `ReActState`：缓存随单次 run 状态构造而生，且不是 transcript 事实；由共享 ReActEngine 按 session 管理更符合其进程内、可淘汰属性。
- 同时收口 Usage mailbox：Usage 有自己的写入与持久化职责，混入本切片会扩大边界。

## 影响与验证

估算路径直接读取当前 `ReActState.canonical`，不克隆整份列表到 mailbox；cache miss、revision/generation 变化和逐出仍走既有安全重建路径。没有数据库、事件、schema、UI 或 wire 迁移；进程重启时缓存自然冷启动。

验证命令：

```text
cargo fmt -p haven-agent -- --check
cargo check --locked -p haven-agent
cargo test --locked -p haven-agent --lib
cargo clippy --locked -p haven-agent -- -D warnings
git diff --check
```

## 回滚

回退本切片并删除本 ADR 与索引项即可恢复 actor mailbox 缓存路径。没有持久化格式或用户数据迁移。
