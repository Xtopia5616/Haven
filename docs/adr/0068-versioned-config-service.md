# ADR 0068：版本化配置服务与运行时应用计划

## 背景

配置文件的 TOML 读写原本由 `ConfigLoader` 提供，但应用状态通过
`Arc<Mutex<ConfigLoader>>` 直接暴露给多个 Tauri command 和 builtin tool。设置保存、模型切换、
MCP/skills 管理和永久授权各自读改写配置，并在保存后分别更新 router、Agent、Tools、MCP、
media、日志和快捷键。

设置表单不拥有 MCP、skills 和 tool settings 字段，因此已有代码还需要在保存前再次从磁盘读取
这些 section，避免一个不完整 payload 覆盖专用命令刚写入的配置。这能防止部分数据丢失，但没有
消除“持久化成功、运行时只应用了一部分”的半完成状态，也没有为配置消费者提供变更版本。

## 决定

1. `haven-common::config::ConfigService` 成为进程内唯一的 live configuration owner。它内部持有
   `ConfigLoader`，提供不可变 `ConfigSnapshot { version, config }`、串行 mutation、原子保存和
   `ConfigChanged { version, domains }` 通知；通知不携带配置值或密钥。
2. 稳定的配置更新优先通过 `ConfigPatch` 的 typed variants 执行；模型角色更新使用
   `LlmRolePatch`。`ReplaceAppConfig` 和 `edit_loader` 只作为迁移期适配入口，调用方不能自行
   调用 `ConfigLoader::save`。
3. `ConfigLoader` 只保留 TOML codec、加载/备份和文件格式边界。原子保存使用同目录、带进程和
   序号的临时文件，避免多个保存者共用固定 `.tmp` 文件。
4. `haven-app-binary::config_runtime::RuntimeConfigApplyPlan` 根据变更 domain 映射 live consumer
   和 `restart_required` consumer。设置命令先提交一个完整配置快照，再按照 plan 更新受影响的
   pipeline、shell、context limits、router、session、MCP、安全、skills、日志和 hotkey；不再
   无条件重建所有运行时组件。
5. `AppState`、Tauri commands 和 `SelfTool` 使用 `ConfigService`。仍需 loader 形状的迁移代码只能
   经过 service 的 serialized adapter；应用代码不得重新暴露共享 loader mutex。

## 替代方案

- 继续在每个 command 中锁 `ConfigLoader` 并手工同步运行时：拒绝，无法保证配置快照、持久化和
  运行时应用的单一权威。
- 只给 `ConfigLoader` 增加版本字段：拒绝，不能阻止多个调用方并发读改写，也不能表达哪些运行时
  消费者需要重建或重启。
- 把完整 `AppConfig` 放进变更事件：拒绝，会扩大 secrets 和隐私泄漏面；消费者可以按 version
  读取同一服务的 snapshot。
- 让 `haven-common` 直接依赖 Agent/Tools 并在下层编排运行时：拒绝，会破坏叶子 crate 的依赖
  方向；运行时 apply plan 留在 app 组合根。

## 影响与验证

- 配置 TOML schema 不变，已有有效配置无需迁移或重置；进程内 version 从 0 开始，重启后重新
  建立 snapshot。
- settings form 的密钥脱敏、permissions 保留、MCP/skills/tool settings 保留语义继续由
  `AppConfig::apply_settings` 和 typed patch 测试保护。
- 运行时尚未支持热替换的 `SkillsExec` 与 Memory maintenance 会在 apply plan 中报告
  `restart_required`，不能静默假装已应用。
- Self/Admin 的第一条迁移已删除 `SelfTool` 的任意 dotted `config_set`；后续 typed
  `ToolOperation` 完成后，应继续删除 `ReplaceAppConfig`、`edit_loader` 和 native
  `SelfTool` 兼容入口。

重点验证：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-common
cargo check --locked -p haven-tools
cargo check --locked -p haven-app-binary
```

## 回滚

回退本 ADR 对应提交即可恢复直接使用 `ConfigLoader` 的调用方；不需要删除数据库或重置用户
配置。若未来改变 TOML schema 或把 version 持久化，必须另立 ADR 并写明配置备份和重置边界。
