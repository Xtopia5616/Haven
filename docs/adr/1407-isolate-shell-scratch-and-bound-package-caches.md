# ADR 1407：隔离 Shell 调用临时目录并限制包管理器缓存

## 状态

已实施；2026-10-25 Rust workspace 测试与 strict Clippy、UI check/1517 项测试/生产构建均通过。Windows MXC Shell 方向权限验收也通过；scratch 容量预算及缓存超限恢复按相关自动测试覆盖。

## 背景

WorkspaceWrite 下，Shell 的 `TEMP` 和 `TMP` 曾被设为第一个授权可写根。该根通常是项目目录，既会将临时产物写进项目，也会让行为依赖授权根的排序。npm、Cargo 等工具还需要可写缓存；把缓存默认放在用户配置目录会落到 MXC 的授权范围之外。

## 决定

1. 每次 Shell 调用创建一个唯一 scratch 目录，`TEMP`、`TMP`、`TMPDIR` 指向该目录；调用没有显式工作目录或检测到的工作区时，当前目录也使用此 scratch。`TempDir` 在调用结束、取消、失败或超时返回时清理；应用重启恢复时清理崩溃遗留目录。
2. 在 WorkspaceWrite 下，授权 owner 将 scratch 作为独立的 MXC 可写根。它不从用户写根列表选取，也不依赖列表顺序。显式 deny 与 protected-read-only 优先；scratch 是满足进程临时文件需求的受限应用目录。
3. npm 缓存位于 Haven runtime Temp 下的 `shell-cache/npm`，Cargo home 位于 `shell-cache/cargo`。环境分别设置 `npm_config_cache` 和 `CARGO_HOME`；WorkspaceWrite 下授权 owner 为它们分配独立可写根。若 Shell 工具文件策略为 ReadOnly，则不授予缓存写入。用户文件写根与执行 cwd 仍遵循原有策略。
4. 容量预算为每次 scratch 256 MiB、scratch 总池 2 GiB、npm 缓存 512 MiB、Cargo home 2 GiB，并将同时运行的 Shell 调用限制为 8 个。运行期间每秒检查本次 scratch；每 15 秒检查 scratch 总池及共享缓存。超限或无法安全检查时终止 Shell 进程并返回错误。扫描统计普通文件的逻辑长度，不跟随符号链接；scratch 条目数达到 100,000、缓存条目数达到 1,000,000 也视为超限。
5. 正常退出由临时目录 RAII 清理 scratch。重启恢复清理所有遗留 scratch。容量超限终止 Shell 后，若没有其它活动 Shell 则立即重置超额或无法安全检查的缓存；若仍有并发 Shell，则等到重启恢复时重置。缓存清理只作用于 Haven 专用目录。

## 配置、数据与兼容影响

不改变配置 schema、数据库、session event 或 IPC。Shell 使用的临时目录位置变为每调用隔离；无工作区且未指定 `cwd` 的命令从其 scratch 启动。npm/Cargo 缓存转移到 Haven runtime Temp 下的独立目录，缓存可能被操作系统临时目录清理，或在超限后的重启恢复中重置。

## 替代方案

- 继续把 Temp 指向第一个用户写根：拒绝，会污染项目且受根顺序影响。
- 让 npm/Cargo 写入用户默认缓存目录：拒绝，MXC 没有显式授权该路径，也不利于独立控制其生命周期。
- 只在命令结束后清理、不设预算：拒绝，单次调用或并发调用仍可能占满临时盘。

## 验收

- 回归验收应确认 TEMP/TMP/TMPDIR 与默认 cwd 位于本次调用 scratch，且不随用户写根顺序变化。
- 回归验收应确认 WorkspaceWrite 将 scratch、npm、Cargo 路径作为独立可写根，显式 deny/protected-read-only 仍优先，ReadOnly Shell 不获缓存写根。
- 容量边界验收应覆盖 scratch、npm 与 Cargo 超限时进程终止、scratch 清理及重启后的过额缓存重置。
- `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、`corepack pnpm run test:run` 与 `corepack pnpm run build` 已通过。Windows MXC 方向权限测试通过；本 ADR 的容量边界仍由自动测试覆盖。
