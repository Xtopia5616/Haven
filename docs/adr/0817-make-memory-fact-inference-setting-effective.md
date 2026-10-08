# ADR 0817：让记忆事实推断设置控制运行时

## 状态

Accepted — 2026-10-08

## 背景

`MemoryConfig.fact_inference_enabled` 已被保存并显示在设置页，但 Agent 构建和 `MemoryWorker` 不读取它，关闭开关后仍会继续发送抽取请求。durable outbox 将节流和真实失败都表示成 `false`，因此被节流的 marker 也会进入短退避重试；暂停触发的 `bypass_throttle` 又会随失败持久保留，使重试继续绕过间隔。LLM 错误被替换为通用文本，排障日志无法区分认证、网络、解析和存储错误。

## 决定

1. `fact_inference_enabled` 在 Agent 启动和 Settings live-apply 时进入唯一 `MemoryWorker` 开关。关闭时暂停 durable fact/summary 抽取与维护阶段的 LLM 推断；确定性清理和向量索引维护仍可运行。关闭期间 marker 保留，重新开启时唤醒 outbox 并继续处理。
2. 普通事实抽取返回 `Done`、`Throttled`、`Retryable` 或 `Disabled`。节流按剩余间隔安排下次尝试且不增加失败计数；真正失败使用原有有界退避，并清除 marker 的一次性 `bypass_throttle`。
3. 将底层 LLM 错误及结构化解析位置保留到失败日志，经过公共错误净化后记录；不记录 prompt、transcript 或凭据。
4. 该切片不改数据库 schema、配置格式或 marker 格式；既有 pending marker 在关闭状态下仍可恢复。

## 替代方案

- 将 `fact_inference_enabled` 标记为仅重启生效：拒绝。当前设置服务已有 live-apply 编排，且只需要切换一个线程安全 gate。
- 关闭推断时确认并删除 pending marker：拒绝。关闭模型功能不应丢弃尚未处理的 durable 工作。
- 所有 false 结果继续共用一次失败退避：拒绝。节流不是 provider 失败，不能消耗失败重试计数或放大请求频率。

## 影响与验证

Settings 中的记忆开关现在即时生效。失败 marker 保持持久化，且退出/重启语义不变。回归覆盖关闭期间不调用模型且保留 marker、重新开启后排空 marker、节流 outcome 和 pause retry 清除 bypass。

适用门禁：`cargo fmt --all -- --check`、workspace Rust check/test/strict Clippy、UI check/test/build 与 IPC contract 检查。

## 回滚

回滚 Agent gate 与 apply phase 会恢复旧的无效设置语义；无需重置配置或数据库。
