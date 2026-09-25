# ADR 0340：Recording event contract audit

- 状态：已采纳（2026-09-25）
- 范围：`crates/app-binary/src/events.rs`、`ui/src/lib/contracts/recording.ts`、`ui/src/lib/events.ts`
- 关联：[ADR 0330](0330-session-lifecycle-ui-contract-mapper.md)、[ADR 0335](0335-action-board-ui-contract-mapper.md)

## 背景

Phase 8 将 recording events 列为待收口的手写 mirror。本切片核对 Rust wire DTO、前端 contract/mapper、
`events.ts` listener、`+layout.svelte` 调用方和现有测试，判断 recording 域是否像 ADR 0330/0335 一样
重复维护了 wire payload shape。

## 审计结论与决定

1. `events.rs` 中的 `RecordingEvent`、`VadStatusEvent`、`TranscriptionStartedEvent`、
   `TranscriptionResultEvent`、`TranscriptionErrorEvent` 和 `RecordingErrorEvent` 是 Rust/Tauri wire DTO 权威。
2. `contracts/recording.ts` 没有平行的 snake_case wire interface。`RecordingPayload`、
   `VadStatusPayload`、`Transcription*Payload` 和 `RecordingErrorPayload` 是调用方消费的 camelCase DTO；
   同一录音开始/停止 payload 已共用 `RecordingPayload`，两种错误共用 `RecordingErrorPayload`。
3. `mapRecordingEvent` 是唯一的 snake_case → camelCase 字段映射点。`recordingEventListeners` 是
   listener 入口并调用该 mapper；`+layout.svelte` 只消费映射后的 DTO，没有第二套字段读取或转换。
4. 因此不提取新的 validator/mapper，也不改生产代码。保留 `RECORDING_EVENT_NAMES` 作为前端订阅登记，
   由现有 IPC channel 检查脚本对照 Rust channel 集合。

## 必须保持的不变量

- Rust channel、DTO 字段、`snake_case` wire payload、`rec-*` 关联及生产者顺序不变。正常路径仍是
  `recording:started`、`recording:stopped`、`transcription:started`，随后以 `transcription:result` 或
  `transcription:error` 结束；录音开始前失败仍走 `recording:error`。
- Mapper 保留 Tauri event name/id envelope，并只输出已知 UI 字段；未知附加字段不会进入 route-facing DTO。
- `VadStatusEvent.signal/state` 在 Rust DTO 中是 `String`，前端保留未知字符串，不将它们改成封闭枚举或
  丢弃未知值。
- 保留当前畸形值降级：布尔字段仍使用 `Boolean` 语义，非法/缺失的字符串及 duration 使用既有安全默认，
  类型无效的可选字段被省略，合法 `confidence` 原样映射。此切片不改错误文本或错误通知路径。
- `recordingEventListeners` 对每个到达事件同步调用对应 handler，不缓存、重排或合并事件。
- 不新增全局 codegen，也不改变 ID、持久化、日志或安全边界。

## 替代方案

- 再提取 mapper/validator：没有重复 wire interface 或第二条字段映射路径；新增抽象不会删除权威来源或减少
  维护点，拒绝。
- 将前端 camelCase DTO 删除并让布局直接读 Rust 字段：会把 snake_case 映射扩散到消费方，违反现有边界，拒绝。
- 引入全域 codegen：超出 recording 审计切片范围；未来只有在生成流程覆盖多个稳定域并能保留运行时行为时再评估。

## 影响与验证

本切片只增加 recording mapper/listener 回归测试和文档。测试固定七个 channel、Rust wire fixture 到 camelCase
DTO 的映射、可选字段与 confidence、未知附加字段、未知 VAD 字符串、既有畸形值默认，以及 listener 到达顺序。
Rust DTO、生产 mapper、channel、payload、消费者副作用和事件生产顺序均未修改。

验收命令：

```sh
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
pwsh -NoProfile -File scripts/check-ipc-events.ps1
git diff --check
```

## 回滚

回滚该提交即可删除新增回归覆盖、ADR 和对应架构/路线图说明。无 Rust wire、配置、schema 或用户数据迁移。
