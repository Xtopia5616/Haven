# 0896：隔离 App 的 Skill 执行与 live-output 能力

## 状态

已接受并实现（2026-10-10）。

## 背景

`AppServices` 已将 ToolRun 变成 App-owned port，但仍把 `Arc<RwLock<SkillRunner>>` 和 `LiveOutputHub` 具体类型交给 App 命令与 bootstrap。命令直接持读锁运行 Skill 子进程，bootstrap 直接安装 live-output sink。

App 只需要发起 Skill 执行并提供事件 sink；它不需要持有 runner 的可变访问锁，也不应依赖 Tools live-output hub 的实现类型。

## 决定

- App 定义 `AppSkillExecutionPort`，封装 Skill 子进程执行；App 命令只传入 registry 解析得到的 `Skill`、参数和取消令牌。
- App 定义 `AppLiveOutputPort`，封装 live-output sink 安装；Tools 仍负责 preview 生命周期和流事件产生。
- `ApplicationRuntime` 保存这两个 port，不再把 `SkillRunner` 或 `LiveOutputHub` 放进 `AppServices`。
- `McpManager` 与 `SkillRegistry` 作为明确的领域 owner handle 保留；其职责与内部 client/map 的所有权继续由各自 crate 封装。

## 影响

- Skill 执行仍由同一 `SkillRunner`、配置快照和取消令牌处理；App 侧错误传播与 UI response 保持不变。
- `LiveOutputHub` 仍是唯一实时 preview 与 live-output 产生方；adapter 只安装 sink，不复制 channel 或预览状态。
- `AppServices` 的其余具体句柄以及 `ToolsFacade.share_services()` 的 asset 用法仍待 §5.7 审查；没有把本 ADR 视为全仓边界审计完成。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- App 继续持有 `SkillRunner` 和 `LiveOutputHub`：拒绝，因为 App 调用方会绑定 runner 锁与 live-output 具体实现，而它们不是 App 的状态 owner。
- 将 Skill 执行挪入 `SkillRegistry`：拒绝，因为目录发现/管理与受限子进程执行是不同职责，合并会模糊安全执行边界。
