# ADR 0661：统一 Session 步数预算字段命名

## 背景

`SessionConfig.max_steps` 表示每次 ReAct run 获得的步数预算；`session_max_steps` 表示持久 Session 跨暂停/恢复 run 累计可达到的最大绝对 `step_number`。两项配置拥有不同统计作用域，但当前命名一项没有作用域、一项把 `session` 放在前缀，导致配置、Agent setters、UI 与日志不成对。

## 决定

- 配置字段统一为 `max_steps_per_run` 与 `max_steps_per_session`。
- Agent runtime field、setter、run budget、日志字段与 Settings UI 使用同一组作用域名；绝对步数终点命名为 `max_allowed_step_number`。
- Settings wire contract 由 Rust 配置 DTO 生成，按新字段重生成，不保留旧字段 alias。
- `SessionConfig` 的 `deny_unknown_fields` 继续生效；旧 TOML key `max_steps` / `session_max_steps` 会使配置解析失败，需编辑或重建 `config.toml`。数据库不变，无 DB reset。
- 单次 run 预算、session 累计 cap 的计算、默认值和生效时序不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`
- `scripts/check-ipc-contracts.ps1`

## 回滚与重置

代码回滚需同步恢复 TOML key、生成 IPC 类型、Agent setter 和 UI 名称。使用旧二进制前需手工恢复配置 key；本次无数据库重置要求。
