# ADR 0547：移除 ConfigApplyGate 过渡别名并命名 coordinator 字段

## 状态

已采纳并实施；应用 crate 检查与工作区 Rust 门禁通过。

## 背景

`ConfigApplyGate` 是 `RuntimeConfigCoordinator` 的同类型别名，最初在 ADR 0253 为保留组合根字段名而留下。当前 `ApplicationRuntime` 持有的对象不只是 mutex：它提供共享锁、模型配置持久编辑，以及 Router/media runtime 的 prepare/publish。与此同时，`RuntimeServices` 和 Tools `AdminContext` 中的 `config_apply_gate` 才是原始共享 `Arc<Mutex<()>>`。同名使调用点看不出自己拿到的是锁还是协调对象。

## 决定

1. 删除 `ConfigApplyGate` 内部类型别名，`ApplicationRuntime` 直接持有 `RuntimeConfigCoordinator`。
2. 将 `ApplicationRuntime` 字段和其调用点统一命名为 `config_runtime_coordinator`。
3. 保留原始 `RuntimeServices` / AdminContext 共享 mutex 的 `config_apply_gate` 名称，因为它们表示的确是同步原语。
4. 在命名规范中区分 `Gate` 与 `Coordinator`。

## 替代方案

- 把所有成员都改成 `config_runtime_coordinator`：拒绝。原始 mutex 没有 coordinator 的 prepare/publish 职责。
- 继续保留别名，仅改字段名：拒绝。无外部消费者的 crate 私有别名仍给类型导航提供第二个名字。
- 将整个 Settings phase 流程迁入此 coordinator：拒绝。`SettingsRuntimeApplyCoordinator` 继续拥有 settings phase 次序/失败观测，当前 coordinator 只管理共享配置 gate、模型编辑与 Router/media 应用。

## 影响与验证

- 仅更名 app-binary crate 内部字段/别名及调用点；共享 mutex、临界区范围、配置提交与 live apply 顺序不变。无 IPC、配置、数据库或用户数据变化。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复 `ConfigApplyGate` 类型别名、`ApplicationRuntime.config_apply_gate` 字段和旧调用点，并移除此 ADR 与当前路线图/命名规范记录。无需运行时配置或数据迁移。
