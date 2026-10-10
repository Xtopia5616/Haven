# ADR 0867：直接使用 Common 错误文本清洗器

## 状态

Accepted — 2026-10-10

## 背景

`haven-common::error::sanitize_error_text` 是跨 crate 的统一错误文本脱敏、单行归一与长度上限实现。App `logging.rs` 曾提供同签名薄转发，通知、事件桥、handlers、commands、config runtime 与 `log_err` 均通过该 App 别名调用；其它 App 路径已经直接调用 Common。该 wrapper 不增加 App 策略，形成两条符号入口和不一致的调用风格。

## 决定

- 删除 `logging::sanitize_error_text` wrapper；App 所有调用点直接使用 `haven_common::error::sanitize_error_text`。
- `logging.rs` 继续独占 tracing subscriber 初始化、命令错误上下文与 `log_err` 的双行日志行为；Common 只拥有错误文本清洗算法。
- 删除 App 对 Common sanitizer 行为的重复 unit checks；安全脱敏与边界行为由 Common 实现与其测试负责。
- 不改变清洗次序、脱敏规则、长度、日志级别、事件/命令语义或用户反馈；不增加跨 crate API。

## 替代方案

- 保留 App wrapper 作为 logging facade：拒绝。`sanitize_error_text` 也服务通知和 IPC event，且 App 没有额外策略；直接依赖 Common 与其它 crate 一致。
- 将 `log_err` 移到 Common：拒绝。命令上下文、tracing 字段和双行输出属于 App composition root。
- 为 notification/event/log 定义不同 sanitizer：拒绝。它们共享同一不可信错误文本边界与脱敏契约。

## 影响与验证

App 不再有第二个 sanitizer 符号入口；命令 logging 与数据清洗的职责边界明确。workspace、测试目标编译及严格 Clippy 通过；测试套件未执行。无 IPC、配置、数据库或持久化变化，不需要重置。

## 回滚

若某个 App 边界未来需要不同清洗策略，应创建表意明确的 App transformation，并说明其输入/输出安全契约；不要恢复无行为转发符号。当前无持久数据需要重置。
