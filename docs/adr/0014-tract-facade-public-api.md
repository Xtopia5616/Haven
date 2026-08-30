# ADR 0014：使用 tract facade 作为 ONNX 推理公共 API

## 背景

Haven 的 Silero VAD 直接依赖 `tract-onnx`，并在业务代码中使用
`TypedSimplePlan`、`TypedFact` 和 `TypedOp` 等 tract 内部类型。tract 0.23
已将 `tract` facade 定义为稳定的 Rust 公共 API，内部 crate 不再是稳定契约。

## 决定

- 工作区只依赖 `tract` facade，使用其官方默认 feature 配置；VAD 的模型执行仍使用 CPU runtime。
- VAD 通过 `tract::prelude::*` 使用 `Runnable`、`State` 和 `Tensor`。
- `Runnable::spawn_state()` 创建并持有 tract 的执行状态，跨音频帧复用。
- Silero v5 的 recurrent tensor 仍由 VAD 单独持有：作为第三个模型输入，并从第二个输出更新。
  这与 tract 的执行状态是两个不同层次，重置时同时重建。

## 影响与替代方案

迁移后调用方不再绑定 `tract-onnx` 或 `tract-core` 的类型，后续 tract 0.23
补丁版本可由 lockfile 固定。代价是采用 0.23 facade 的加载与执行 API；不保留旧
内部类型兼容层，因为它们不是稳定契约。继续直接依赖 `tract-onnx` 可减少一次
短期改动，但会继续把内部 crate 类型传播到业务代码，因此不采用。

## 验证与回滚

使用以下命令验证模型加载、连续帧 state 传递、reset 和 workspace 测试：

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --locked -p haven-input
cargo test --workspace --locked
```

若需回滚，仅回退本 ADR、工作区依赖和 `crates/input/src/vad.rs` 的对应提交；不涉及
数据库、配置、快照或用户数据重置。
