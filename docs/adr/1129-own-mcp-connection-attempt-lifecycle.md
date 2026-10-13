# ADR 1129：由 MCP manager 统一拥有连接尝试生命周期

## 背景

MCP 配置加载、动态连接、手动 reconnect 和自动重连共用连接并发预算（ADR 1113），但连接尝试的取消、失败清理和客户端替换仍需要跨调用方与异步 worker 协调。相同 server name 的并发尝试可能互相覆盖；旧尝试的延迟清理也不能误删较新连接。网络策略可能在等待准入或建立连接期间改变，shutdown 必须停止新准入并回收已有 worker。

## 决定

1. `McpManager` 是连接尝试的唯一生命周期 owner。每个 server name 同时至多有一个 reservation；reservation 带 generation、目标 client 和取消 token，配置加载、动态连接、手动 reconnect 与自动重连共用有界 admission。
2. `NetworkPolicy::Deny` 在等待连接槽位前快速失败；创建 reservation 和提交已连接 client 时仍在串行化的 lifecycle gate 内重新检查策略与 manager shutdown 状态。策略变更期间不得把未获准的 client 发布到可用集合。
3. 调用方等待由 manager 持有的连接 worker 结果。等待方被取消或丢弃时取消对应 reservation；普通失败通过代次校验更新离线状态，panic、取消和过期成功通过 generation/client 校验清理。旧 worker 不得清除新一代连接、listener 或 monitor。
4. `shutdown_all` 永久关闭连接准入，取消活动 reservation，并有界等待连接 worker；随后停止并 join 对应 monitor、notification listener 和 client。manager shutdown 后不支持重新启动。
5. 连接状态与 reservation 是运行时数据，没有数据库、配置、IPC 或 provider wire 格式变化。相同名称重复连接按当前 generation 的结果收敛，错误文本仍走既有清洗边界。

## 替代方案

- 让每个入口各自创建与清理 worker：拒绝。多个入口会产生重复的名称互斥、取消和 shutdown 规则，且难以保护新连接免受旧清理任务影响。
- 仅用 semaphore 限制并发：拒绝。容量限制不表达同名尝试唯一性、尝试 owner、策略重检或 shutdown admission fence。
- 让 caller task 直接持有连接建立 future：拒绝。caller abort 会遗留不完整状态，或令配置加载与 manager shutdown 无法可靠收割后台工作。
- 按行数拆分 manager：拒绝。新生命周期状态需要共同访问私有 reservation、generation 和运行任务句柄；当前没有稳定的跨模块或 integration-test seam。体量本身不足以证明新的 owner 边界。

## 实现边界与后续拆分条件

reservation registry、connection worker registry 和 lifecycle gate 暂留在 `manager.rs`，生命周期竞争测试也留在 manager 私有测试模块，以便验证具体代次、取消和 shutdown 状态而不扩大生产可见 API。文件超过常规体量是已知约束；若后续出现稳定的 connection lifecycle 子对象或可复用的私有测试 seam，再把生命周期实现与 loopback HTTP fixture 分到独立模块。拆分应保持 manager 对外 API 与所有权不变，并用竞态回归覆盖验证。

## 验证

- MCP 定向测试覆盖初始连接 panic/等待者取消、动态连接 Deny 与取消、同名重复连接、旧代次延迟清理、shutdown drain/admission fence、连接槽耗尽时 Deny 快速失败，以及现有连接准入限制；`cargo test --locked -p haven-mcp` 通过（70 项）。
- `cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。
- 连接生命周期只影响 MCP manager 运行时；本 ADR 不改变 durable event、数据库 schema 或外部协议。

## 回滚

恢复 manager 之前的连接入口与 worker 清理实现即可。没有持久状态需要迁移或清理；若回滚代码，须一并恢复对应的生命周期竞态测试与准入行为。
