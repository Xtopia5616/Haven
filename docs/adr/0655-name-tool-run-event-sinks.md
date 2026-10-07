# ADR 0655：区分 ToolRun 生命周期与前台实时输出 sink

## 背景

Tools 暴露两种 UI event callback：`ToolRunService::set_event_sink` 接收 typed `ToolRunLifecycleEvent`，用于可持久运行的 background/scheduled ToolRun；`LiveOutputHub::set_event_sink` 接收 raw channel 与 JSON payload，用于前台工具运行中的 `agent:tool_output` 预览。两者在 App bootstrap 同时安装，名称相同却对应不同 event contract 和消费时点。

## 决定

- lifecycle callback 类型改名为 `ToolRunLifecycleEventSink`；ToolRunService setter 改为 `set_lifecycle_event_sink`。
- 前台预览 setter 改为 `set_live_output_event_sink`，hub 内部字段也标出 live-output owner。
- ToolRunService 内部 lifecycle sink state 改名为 `lifecycle_events`；不合并双通道，不统一成 raw event/payload 或新的 generic sink。
- 删除旧公开名字，不保留兼容 alias；事件名、payload、发布时机及 lifecycle 状态不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --locked -p haven-tools`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚与重置

代码回滚时恢复 `EventSink` / `set_event_sink` 名称。本次仅修改 Rust API 名称，不改变 IPC、数据库、配置或用户数据，不需要重置。
