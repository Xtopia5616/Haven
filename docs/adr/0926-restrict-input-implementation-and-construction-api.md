# 0926：收回 Input 实现模块与无效构造入口

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven-input` 的 `capture`、`vad`、`hotkey` 模块原本均公开。下游只需要 `KeyCode`、`KeyCombo`、VAD 信号/状态和 `InputPipeline`，但模块可见性还额外暴露 CPAL backend、capture engine、ring buffer、resampler、VAD engine/detector 等实现路径。调用方因此依赖文件结构而非明确的输入采集契约。

快捷键 modifier 以原始 `u8` bitmask 和四个公开常量表示，`KeyCombo::new` 接受任意 bits；调用方可以构造 parser 不会产出的组合。`RecordingResult::default()` 产生空 PCM、零时长且原因却为 `Manual` 的伪结果，只被测试用作部分初始化 shortcut。`InputPipeline::default()` 则创建合法的空闲管线，是 `new()` 的标准无参默认构造；该契约继续保留。

## 决定

- 将 `capture`、`vad`、`hotkey` 改为 crate 私有模块；跨 crate 消费者统一从 `haven_input` 根导入。
- 根模块只导出实际跨 crate 契约：`AudioConfig`、`KeyCode`、`KeyCombo`、`KeyModifier`、`VadSignal`、`VadState`、`InputPipeline`、`InputEventHandler`、`RecordingState`、`RecordingReason` 与 `RecordingResult`。engine、detector、ring、resampler 和 capture handle 不进入下游 API。
- 用 `KeyModifier::{Control, Shift, Alt, Super}` 代替公开 bit constants；`KeyCombo::has_modifier` 接受强类型。移除未消费的公开 `KeyCombo::new` 与 bitmask getter，使合法组合只能由 `KeyCombo::parse` 创建。
- 保留合法的 `InputPipeline::default`，以其默认状态创建空闲管线；移除会制造伪录音的 `RecordingResult::default`，测试显式填写结果字段。
- 删除生产未使用的 resampler 一次性 wrapper/reset 和 RingBuffer RMS 查询。数字静音检测实际由设备回调的信号阈值判定，RMS 方法及其文档描述属于未接入实现。
- 不保留旧 module path 或 raw modifier API 的兼容入口。输入采样、VAD 判定、快捷键解析语义、IPC、配置和持久化行为不变。

## 影响

这是 workspace 内未发布 Rust API 的破坏性收口。其他 crate 已迁移到 root API。外部调用方若依赖实现模块、raw modifier bits 或测试构造器需改用根导出类型与 `KeyCombo::parse`。没有 IPC、配置、数据库或运行时采集语义变化；无需数据重置。

## 验证

- 静态搜索确认其他 crate 不再引用 `haven_input::{capture,hotkey,vad}::`。
- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-input`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 保留公开模块并要求代码审查只用根 re-export：拒绝，编译器仍接受重复导入路径。
- 保留 `u8` 并只隐藏 modifier 常量：拒绝，任意 bitmask 构造仍可产生 parser 不会产生的状态。
- 保留空 `RecordingResult` 的 `Default` 并仅在测试中使用：拒绝，该默认值会把空 PCM 标成 `Manual` 录音；测试应显式填写 fixture。`InputPipeline::Default` 保留，因为其结果是有效的 idle owner。

## 回滚

重新公开三个模块、modifier bit constants 与 `KeyCombo::new`，并恢复 `RecordingResult::default` 即可；`InputPipeline::default` 仍保留。若 RMS 查询未来出现真实采集 owner 需求，应先把判定接入实际静音策略，再重建该方法和测试；当前只恢复未使用方法不能改变静音行为。
