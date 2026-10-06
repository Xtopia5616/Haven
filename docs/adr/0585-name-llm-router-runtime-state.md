# ADR 0585：为 LLM Router 运行态使用具名初始化结果

## 状态

已采纳并实施。

## 背景

`LlmRouter::runtime_state` 为 router 构造准备 endpoint health、stream rules、model semaphores 和 rate-limit cooldown 四个运行态对象，原先通过 `RuntimeStateParts` 位置 tuple 返回。生产构造器必须忽略第二项，再单独创建一个空 stream-rules lock；测试构造器才直接使用这项。tuple 顺序隐藏了每个共享状态的角色，也导致生产路径重复创建同一类初始状态。

## 决定

1. 用 `LlmRouterRuntimeState` 结构体替代 `RuntimeStateParts`，字段与 `LlmRouter` 的运行态 owner 对应。
2. 生产和测试构造器均从该结构体按字段接收所有初始化状态。
3. 保持 stream rules 初始为空、health 模型集合、semaphore limit 与 rate-limit cooldown 初始化规则不变。

## 替代方案

- 保留 tuple 并只给局部变量改名：拒绝，tuple 顺序仍是结构唯一的字段映射；生产仍会重复创建 stream-rules lock。
- 用 `LlmRouter` 本身承载构造阶段：拒绝，router runtime owner 与一次性初始化结果生命周期不同，且会混淆构造过程和最终对象。

## 影响与验证

- 更新 LLM router 的运行态构造结果和 production/test constructor consumers。
- 不改变 provider 路由、限流、并发容量、stream rules 或外部 API；无持久化、配置、IPC 或安全契约变化，无需重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-llm`、`cargo clippy --locked -p haven-llm -- -D warnings`、`cargo test --locked -p haven-llm -- --test-threads=1`（508 项通过，1 项手动性能测试忽略）、ADR 索引及 `git diff --check`。

## 回滚

将 `LlmRouterRuntimeState` 还原为 `RuntimeStateParts` tuple，并恢复构造器 tuple 解构；外部契约无需变更。
