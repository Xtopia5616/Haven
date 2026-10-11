# 0962：对 ToolResult 资产与录音事件进行失败关闭校验

## 状态

已接受并实现（2026-10-11）。

## 背景

`ToolResultEnvelope.assets` 从动态工具输出收集资产引用；MCP 等输出可能携带任意 `asset_id` 字符串。Rust collector 与 UI mapper 此前只检查字符串类型，因此非 `asset-*` 值可以进入 observation envelope。

Window 与 Media builtin renderer 也会显示动态输出中的 `asset_id`；此前专用 renderer 同样只检查字符串类型，畸形值会被呈现为受管资产身份。该路径只显示文本，不会解析成资产句柄或传入媒体操作。

录音事件 mapper 也把不同契约合并处理：`recording:stopped` 缺失或未知的 `reason` 会被静默省略；必需的错误文本和转写结果字段类型不符时则被替换为空字符串或零。Rust producer 已把 stop reason 定义为闭集，并总在 stopped 事件提供它；`TranscriptionResultEvent.duration_ms` 是 `u64`。

## 决定

- Tools 只把 Common canonical checker 接受的 `asset-*` 值收集进 `ToolResultEnvelope.assets`；UI observation mapper 对数组每项再次校验，任一无效引用会拒绝整个 observation。
- Window 与 Media builtin renderer 只在顶层或 `media.asset_id` 为 canonical `asset-*` 时使用专用视图；无效值退回通用 JSON 视图，保留整个动态输出供检查。MCP/Skill 等扩展 renderer 继续使用开放 JSON contract。
- `recording:started` 的 reason 可省略；若存在则必须是 generated stop-reason 枚举值。`recording:stopped` 必须携带该闭集中的 reason，否则 UI mapper 丢弃事件。
- `recording:error` / `transcription:error` 必须提供 string `error`；`transcription:result` 必须提供 string `text` 和非负安全整数 `duration_ms`。缺失或类型不符时丢弃事件，不合成默认业务结果。
- 空转写文本与零时长仍然合法；VAD malformed-value defaults 保持 ADR 0694 规定的兼容行为。
- 保持 Rust wire DTO 与 JSON 形状不变；此 ADR 只收紧生产投影和 untrusted wire 解析。

## 替代方案

- UI 继续仅验证字段存在或字符串类型：会让错误实体身份进入资产列表。
- 将未知 stop reason、错误或时长转换为空值/零：会把损坏的 wire payload 伪装成正常领域结果，并可能改变录音生命周期。
- 把整个动态 ToolResult.output 改成封闭 schema：当前工具输出没有共同 producer schema，超出本问题边界。

## 影响

符合现有契约的合法 payload 和用户可见行为不变。畸形 asset reference 不进入受管资产列表，也不会被专用 renderer 标为受管资产；该结果仍以 JSON 展示。缺失/错误类型的必需录音事件字段会被过滤。无数据库、配置或 Rust DTO 迁移要求；事件监听器已有隔离边界，丢弃 malformed event 不会阻止其它 channel 更新。

## 验证

- Rust 回归用例覆盖 nested assets 中错误前缀、非规范 UUID32 与有效值。
- UI 回归用例覆盖坏资产 ID、缺失/未知 stop reason、错误类型、负数/小数/不安全 duration，以及空文本和零时长合法路径。
- Builtin renderer 回归用例覆盖 Window/Media 顶层与嵌套资产身份；扩展 renderer 仍保留开放 JSON 路径。
- `cargo test --locked -p haven-tools tool_contract::tests::result_envelope_keeps_recovery_metadata_and_asset_handles`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

若未来录音协议明确允许扩展 stop reason 或改变必需字段，应先更新 Rust enum/DTO、generated TypeScript contract 与 mapper 测试，再调整这里的解析规则；不可恢复将畸形 payload 静默转换成空字符串或零。
