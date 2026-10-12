# 1033：区分 durable event 顺序与流式分片序号

## 状态

已采纳并实施（2026-10-12）。

## 背景

Agent Tauri event 用 `event_seq` / `eventSeq` 表示产生投影的 `session_events.sequence`，流式分片另用 `seq`。短名相似但排序域不同，UI 去重状态也沿用 `eventSeq`。

## 决定

- durable event projection 字段统一使用 Rust/wire `event_sequence`、UI `eventSequence`；对应 reducer 状态名也使用完整词。
- streaming chunk 的 `seq` 保持原名，用于单个 stream 内部分片排序。
- `event_sequence` 仅在 committed event replay 时从 `SessionEvent.sequence` 得出；它不写入 `TranscriptRecord` 或其 UI annotation payload。数据库列及 `SessionEvent.sequence` 保持不变。

## 影响

Tauri event payload 字段名发生同步变化，UI 与 Rust 同版本发布。没有数据库 schema 或 durable JSON 迁移；不接受旧 IPC 字段 alias。

## 验证

- Agent/App focused serialization 与 bridge tests
- UI event contract、session reducer 与 stream aggregation focused tests
- `cargo fmt --all -- --check`、`corepack pnpm run check`
- `scripts/check-ipc-events.ps1`、`git diff --check`

## 回滚

回滚时仍须在代码与文档中区分 event-log 顺序与流式 chunk 序号，避免把二者再统一缩写为 `seq`。
