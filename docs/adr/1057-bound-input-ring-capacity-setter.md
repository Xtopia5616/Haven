# ADR 1057：约束 Input ring capacity setter

## 状态

已接受（2026-10-12）

## 背景

Common 配置校验限制 `context_limits.input_ring_buffer_secs` 不超过 120 秒，目的是约束环形缓冲分配。`InputPipeline::set_ring_buffer_capacity_secs` 是跨 crate 公共 API，却直接保存任意 `usize`；值在 capture engine 创建时乘以 16 kHz 采样率并分配 `Vec<f32>`，调用方可绕过 Common 的资源上限。

## 决定

- Setter 返回 `anyhow::Result<()>`，仅接受不超过 Common 所有的 `MAX_INPUT_RING_BUFFER_SECS` 的值。
- 越界输入返回错误且不修改先前容量；拒绝而不 clamp，避免配置值与 runtime 值静默分歧。
- 保留 0 的既有含义：capture engine 使用默认 20 秒容量。上限和错误校验不改变 0–120 秒现有行为。
- App 初始化传播 setter 错误；Settings apply 将 setter 错误记为当前 phase 失败，不继续应用该 phase 的 Tools/Agent context limits。
- 容量变更仍只影响下一次 capture engine spawn，运行中的 engine 不 resize。

## 验证

- Input 单测覆盖 0、120 接受，121 与 `usize::MAX` 拒绝，以及拒绝时不覆盖原值。
- 完整 Rust workspace 检查、Clippy 与测试通过。
