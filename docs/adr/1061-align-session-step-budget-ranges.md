# ADR 1061：统一 Session 步数预算的有效范围

## 状态

已接受（2026-10-12）

## 背景

Settings 控件将每次 ReAct run 预算限制在 1–1000，将 Session 生命周期累计预算限制在 0–100000；累计预算输入 0 会写成 `None`（不限）。TOML 与 Settings 共用的 Common validator 只检查整数可安全传过 JavaScript，没有应用这些字段范围。于是配置中的 `max_steps_per_run = 0` 或 `max_steps_per_session = 0` 可以被接受，并使 ReAct 执行范围为空；过大值也会让 TOML、IPC 与 UI 编辑能力不一致。Agent runtime setters 同样接受无效值。

## 决定

- `max_steps_per_run` 统一限制为 1–1000。
- `max_steps_per_session` 使用 `None` 表示不限，配置值限制为 1–100000。Settings UI 保留 0 作为不限输入，并在保存前映射为 `None`；wire/TOML 的显式 0 拒绝。
- Common 提供共享预算 validator，供 TOML/Settings 校验和 Agent runtime setters 使用；UI Settings parser 与数字控件共用一组前端边界常量。

## 影响

不会产生零步运行；UI 可表达的配置与 TOML、IPC 及运行时 setter 一致。现有默认值 500 / `None` 不变。数据库、会话事件和序列化格式不变，不需要重置数据库。

## 验证

- Common 测试覆盖每项上下界、0 和超界值，并分别验证 AppConfig 与 Settings。
- Agent setter 与 UI parser 的范围测试通过；数字控件使用 parser 共用的边界常量。
- Workspace Rust 检查、clippy、全量测试，UI check 和全量测试通过。
- ADR 索引与本地链接检查通过。
