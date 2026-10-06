# ADR 0592：为 Tools operation attributes 使用具名结果

## 状态

已采纳并实施。

## 背景

Tools 根据 operation 名称与并发策略推导三个彼此独立的属性：effect、data sensitivity 和 network access。`operation_attributes`、`operation_attributes_for_input` 以及 builtin 层的 `operation_policy_attributes` 都以相同顺序返回 tuple；默认 Tool、原生入口、builtin operation policy 与 operation-view catalog 依赖位置解构，测试也要记住网络访问是 tuple 第三项。

## 决定

1. 在 Tool contract owner 中定义 `OperationAttributes { effect, data_sensitivity, network_access }`。
2. operation-name、input-aware 与 builtin override helper 都返回该具名结果。
3. 输入 contract 的 `read_only` 仍覆盖推导 effect；显式 network override 仍覆盖推导值；confirmation 仍只依据 risk 和 effective effect 决定。

## 替代方案

- 分别改写各 caller 并保留 tuple：拒绝，索引/解构位置仍是共享安全规则的一部分，容易在新入口中错配。
- 将 `OperationAttributes` 直接并入 `OperationPolicy`：拒绝，属性推导与最终 policy 组装由不同层负责，builtin override、risk、capability、confirmation 等字段只在 policy owner 合并。

## 影响与验证

- 改动限于 `haven-tools` crate 内部 Rust 结果形状，不改变 manifest、IPC、事件、JSON、授权策略或用户行为。
- 更新命名路线图；无需持久数据重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`cargo test --locked -p haven-tools`（799 unit + 7 integration passed / 2 ignored）、ADR 索引与 `git diff --check`。

## 回滚

恢复三个属性 helper 的 tuple 返回与调用方位置解构；不涉及持久数据。
