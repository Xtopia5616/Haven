# ADR 1314：收拢 App command DTO 与 detached process helper

## 背景

`commands/contracts.rs` 同时保存命令安全 metadata 和只由 Memory、Skills、Tools handler 使用的 response DTO。Memory response-shape generation 与 IPC checker 还按旧 DTO 所在路径读取类型。另有 `commands/external.rs` 的 `spawn_detached` 和 `open_skills_dir` 各自获取同一个 Platform process-spawn lock 并启动 detached host process。

Slice 7 还要求复核 AppState/bootstrap/cleanup 是否存在可独立的私有 owner。

## 决定

1. 将 Memory response/cursor/fact DTO 及其序列化测试放入 `commands/memory.rs`；将 Skill response DTO 放入 `commands/skills.rs`；将 sandbox review 与 builtin manifest DTO 放入 `commands/tools.rs`。`commands/contracts.rs` 继续拥有 command boundary/security metadata。
2. 更新 IPC generator 的 Memory DTO 路径和 contract checker 的类型位置。通过官方生成器更新 `generatedCommands.ts`；该变更只调整派生类型声明的排序，响应字段、Serde 规则和 IPC wire shape 不变。
3. 新增私有 `commands::process::spawn_detached`，集中使用 Platform process-spawn lock 并丢弃 detached child handle。`external` 和 `open_skills_dir` 共用该 helper；每个调用方继续拥有授权、目标校验、可执行文件选择、参数构造及错误上下文。
4. 保留 AppState/runtime 与 `bootstrap::run` 的单一 composition/lifecycle owner。AppState 按依赖顺序装配 stores/services/runtime，并注册由 runtime cancellation 和 task tracker 管理的 cleanup/recovery 工作；bootstrap 按 Tauri setup、事件、窗口生命周期及命令注册顺序完成组合。拆分 cleanup/helper 仅搬代码，没有第二状态 owner、独立生命周期消费者或更窄稳定接口作为依据。出现独立 owner 或实际重复策略时再复核。

## 替代方案

- 继续把多域 DTO 留在 `contracts.rs`：拒绝；唯一消费者已经按 command domain 分组，metadata 与响应投影无共享生命周期。
- 让 Skills 继续复制 spawn lock 及启动序列：拒绝；同一低层启动约束会随以后修正发生漂移。
- 将 file manager、authorization 或 path policy 一并移入 helper：拒绝；共享部分仅是受锁保护的 detached spawn。命令各自的授权与目标语义不同。
- 把 AppState/bootstrap 拆成新 Runtime/service container 或按文件长度切分：拒绝；当前 composition 顺序和 task 生命周期没有形成可独立持有的 owner。

## 影响与回滚

此变更只调整 App 内部模块位置和共享的 process-spawn plumbing，不改变 command 注册、Serde/IPC payload、数据库、配置、durable event、权限判定、文件路径处理或进程参数，不需要数据重置。回滚时可将 DTO 放回 `contracts.rs` 并恢复 generator/checker 源路径；process helper 可内联回两个 command owner，保持锁和调用参数不变。

## 验收

- `cargo fmt --all -- --check`、`cargo test --locked -p haven-app-binary`（253 passed）、`cargo test --workspace --locked -- --test-threads=1` 与 `cargo clippy --workspace --locked -- -D warnings` 通过。
- `scripts/check-ipc-contracts.ps1`（85 handlers，含 generator `--check`）、UI `corepack pnpm run check`、`test:run`（161 files / 1337 tests）和 `build` 通过；响应 DTO wire shapes 未变。
- ADR index（1297 unique records）与 `git diff --check` 通过。
- AppState/bootstrap no-split 依据记录在[第一轮源码审计](../refactor-plan-source-audit-2026-10-17.md)与[路线图](../architecture-refactor-roadmap.md)；结构切片清空，Windows 发布仍按 §5.1 单独签核。
