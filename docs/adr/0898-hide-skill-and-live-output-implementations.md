# 0898：隐藏 SkillRunner 与 LiveOutputHub 实现

## 状态

已接受并实现（2026-10-10）。

## 背景

`AppServices` 已使用 App-owned ports 隔离 Skill 执行与 live-output sink，但 `ToolServices` 仍将 `Arc<RwLock<SkillRunner>>` 和 `Arc<LiveOutputHub>` 作为公开字段交给 composition adapter。外部调用者因此仍能直接持有实现类型与 runner 锁。

## 决定

- `ToolServices.skill_execution` 暴露 `SkillExecutionPort`，由 Tools 内部适配器持有 runner 锁并执行子进程。
- `ToolServices.live_output` 暴露 `LiveOutputSinkPort`，由 Tools 内部适配器将 sink 安装到唯一的 `LiveOutputHub`。
- 具体 `SkillRunner` 与 `LiveOutputHub` 仅作为 Tools crate 内部字段保留；App adapter 只依赖 capability ports，并继续实现 App-owned 消费端 ports。
- MCP manager 与 SkillRegistry 仍作为领域 owner handles 暴露；ToolRunService 的跨 crate具体类型仍由 §5.7 后续单独收口。

## 影响

- Skill 配置快照、子进程隔离、取消和结果行为保持不变；调用方不能绕过 execution port 直接借用 runner 锁。
- LiveOutputHub 仍是唯一 preview 生命周期与事件来源；sink 安装通过一个窄接口完成。
- 其他 Tools crate 内的 catalog/build 组合仍访问 owner 持有的具体实例；跨 crate 生产消费者只拿 port。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- 仅保留 App-owned ports、继续公开 Tools 具体句柄：拒绝，因为其他跨 crate 调用者仍可绕过消费端适配层。
- 将 SkillRegistry 与 SkillRunner 合并：拒绝，因为目录管理与受限进程执行是不同 owner 和安全职责。
