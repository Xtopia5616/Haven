# 0941：移除 Input 对 Common AudioConfig 的重复导出

## 状态

已接受并实现（2026-10-11）。

## 背景

`AudioConfig` 是 Common config 的稳定配置模型，Input 只在 `InputPipeline` 内部读取它并在 `update_audio_config` 参数中接受同一类型。Input crate root 曾额外重导出该类型，但 App、Tools、Agent 和测试均使用 `haven_common::config::AudioConfig`，没有通过 `haven_input::AudioConfig` 消费。

## 决定

- 删除 `haven_input::AudioConfig` 根 re-export。
- Input 内部直接导入 owner 路径 `haven_common::config::AudioConfig`。
- 保持 `InputPipeline` 方法与运行行为不变。

## 影响

这是未发布 workspace Rust API 的破坏性收口，只移除没有仓内消费者的源码导入路径。配置结构、Serde、IPC 与录音行为均不变，无需数据重置。

## 验证

- 静态搜索确认仓内没有 `haven_input::AudioConfig` 消费者。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引及 `git diff --check`。

## 替代方案

- 保留 Input root alias 作为方便导入：拒绝。配置类型的定义与语义 owner 是 Common，Input re-export 不添加采集领域约束或新类型。

## 回滚

若 Input 未来需要自己的音频运行配置，应定义有独立语义的 Input-owned type，并在边界显式转换；不恢复共享 DTO 的第二条导入路径。
