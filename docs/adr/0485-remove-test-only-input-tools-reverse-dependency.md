# ADR 0485：移除 Input 到 Tools 的测试专用反向依赖

## 状态

已采纳并实施（2026-10-05）。

## 背景

生产依赖方向是 `haven-tools → haven-input`：Tools 的 Windows key simulation 使用 Input 暴露的 `KeyCode` 与名称解析。`haven-input` 曾通过 dev-dependency 反向依赖 `haven-tools`，唯一用途是在 `hotkey.rs` 中枚举 `simulate::accepted_key_names()` 并检查 Input parser 的兼容性。该测试依赖已经存在的 Tools consumer API，形成测试目标上的反向边，虽不进入生产依赖图或 ADR 0474 的依赖清单，但模糊了模块的测试所有权。

Tools 已有反向兼容性检查：将 Input 的 `KeyCode` 名称传给模拟器的 Windows key map。把另一方向也放在 Tools 的消费侧测试中，可以双向验证真实 API，而无需 Input 依赖上层 Tools。

## 决定

1. 把“模拟器接受的按键名称都能由 Input hotkey parser 解析”的 Windows-only 检查并入 `haven-tools::simulate` 测试，与现有 key map 兼容检查一起覆盖双向契约。
2. 删除 `haven-input/Cargo.toml` 对 `haven-tools` 的 dev-dependency，并同步更新 Cargo.lock 中 `haven-input` 的依赖项。
3. 保持 `KeyCode::parse`、`accepted_key_names`、生产 Cargo 依赖、运行时行为与公开 API 不变；不改 ADR 0474 的生产依赖表，因为它只记录非 dev 内部依赖。

## 替代方案

- 保留反向 dev-dependency：拒绝。它让下层 Input 测试依赖上层 Tools，只为验证 Tools 消费 Input 的兼容性。
- 在 Input 测试中复制一份模拟器名称列表：拒绝。复制会引入第二份按键名称权威，无法直接验证真实消费者 API。

## 验证与影响

确认 workspace 内 `haven-input` 不再引用 `haven-tools`，Tools 的 Windows-only parity 测试保留且涵盖 modifier 例外；运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`pwsh -NoProfile -File scripts/check-crate-dependencies.ps1` 与 `git diff --check`。

只删除测试构建图中的一条反向边；生产依赖图、数据库、配置、IPC、用户数据与安装行为均不变。回滚只需恢复 dev-dependency 和 Input 内 parity 测试，并移除 Tools 对应断言；不涉及迁移或重置。
