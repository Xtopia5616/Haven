# ADR 0854：明确命名工具能力快照投影

## 状态

Accepted — 2026-10-10

## 背景

`ToolsFacade::runtime_capabilities()` 是异步读取入口：它取得当前 `PlatformRuntime`，重新构造
`ToolCapabilitySnapshot`，再返回给 Agent 的 `RuntimeCapabilities`。快照本身的
`runtime_capabilities()` 则是同步纯映射，只把快照字段投影到 prompt/registration 使用的较窄值。
两个不同职责在不同 owner 上使用同一个函数名；快照映射也只供 Tools crate 内的 facade 调用，
却暴露为 crate-wide 方法。

## 决定

- 将 `ToolCapabilitySnapshot::runtime_capabilities` 改名为
  `ToolCapabilitySnapshot::project_runtime_capabilities`，表达它从权威快照派生较窄的消费者视图。
- 保留 `ToolsFacade::runtime_capabilities` 作为 fresh read 入口；快照类型与 `RuntimeCapabilities`
  继续分开，前者供 Tools 的完整能力判断，后者供 Agent prompt/工具注册读取。
- 方法仍保持 crate 内可见，因为 `ToolsFacade` 与 snapshot 位于兄弟模块；不扩大到跨 crate API。
- 不改变映射字段、能力来源、刷新时机、缓存或 `ToolCapabilitySnapshot` 的唯一 owner。

## 替代方案

- 保留两个相同方法名，依赖接收者类型辨别：拒绝。查询与纯投影在调用图和源码搜索中容易混淆。
- 合并两个能力结构：拒绝。`ToolCapabilitySnapshot` 是 Tools 内部完整能力事实，`RuntimeCapabilities` 是面向消费者的窄投影，字段范围与 owner 不同。

## 影响与验证

这是 `haven-tools` 内部方法命名变化；无跨 crate API、IPC/event、配置、持久化或行为变化，无需数据重置。
验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-tools`、
`cargo clippy --locked -p haven-tools -- -D warnings`、ADR index 检查与 `git diff --check`。
未运行测试套件；既有快照映射测试调用已重命名的方法且未改动断言。

## 回滚

若构建或调用点核对发现问题，整体恢复快照方法名及其 Tools facade 调用；不要给两个不同 owner
的操作新增同名兼容别名。
