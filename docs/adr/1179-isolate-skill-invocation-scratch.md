# ADR 1179：按 Skill 调用隔离并回收工作目录

## 状态

已采纳并实施。

## 背景

`SkillsExecConfig.work_dir` 过去同时作为所有 Skill 的进程当前目录。不同 Skill 和并发调用可读写彼此遗留的相对路径文件；相对配置路径还会依赖 Haven 的启动目录。工作目录是脚本方便使用的相对路径基准，但它本身不会限制子进程访问其它文件系统路径。

## 决定

- `skills_exec.work_dir` 定义一个绝对本地 scratch 父目录。Common 配置校验拒绝相对、UNC 和 Windows device 路径；执行前再由 `DirectoryCapability` 打开并拒绝包含 reparse point 的根。
- 每次执行在该根下创建随机、独立的 `TempDir`，并将其作为子进程 `cwd`。正常返回时由 RAII 先尝试删除整个调用目录；若 Windows 进程退出存在短暂句柄释放延迟，再经已打开的父目录 capability 对随机相对目录做最多 8 次、间隔 25ms 的有限重试。超时和取消先请求结束并等待主 Skill 进程退出，再释放 scratch。并发 Skill 不共享相对文件。
- Haven 被强制终止时析构不会运行；如果 Skill 后代仍持有工作目录，Windows 的 Job 关闭也可能使 RAII 删除失败。带 `haven-skill-run-` 前缀的残留目录可能留在配置 scratch 根。当前不在启动时递归删除这些目录；后续需单独定义并验证安全的进程树等待与崩溃回收策略。
- 临时 `cwd` 仅为相对文件操作提供隔离与生命周期 owner，不是 OS filesystem sandbox。Skill 子进程仍按 ADR 0153 在 `FullAccess + Open` 下执行；绝对路径访问与网络能力仍由操作系统和相应安全策略决定。

## 替代方案

- 继续让所有 Skill 共用持久目录：拒绝，会保留跨 Skill 污染、并发冲突及无完成生命周期的问题。
- 将 `cwd` 直接绑定到 `ExecutionWorkspace`：拒绝，当前 Tool execution root 是项目文件工作区；Skill 的过程文件是临时 scratch，不应混入项目文件或暗示获得文件写入授权。
- 把临时 `cwd` 当作文件系统沙箱：拒绝，子进程仍拥有宿主进程的文件和网络权限。

## 影响与迁移

`skills_exec.work_dir` 现必须为绝对本地路径。含相对路径的配置会在语义校验失败时按既有配置恢复流程备份并只读启动；用户可将其改为绝对路径，或按 `docs/release-and-reset.md` 仅重建配置。数据库与技能源文件不变，无需删除。Skill 无法再依赖上一次调用写入其相对 `cwd` 的文件，且本次调用在该目录生成的文件会随调用清除；结构化 JSON stdout 是当前支持的结果通道。

## 验证

- Common 配置测试拒绝相对 Skill work root；Platform 测试证明 capability 递归清理不会跟随嵌套 symlink；Tools runner 测试覆盖 reparse root fail closed，以及成功、失败、超时和取消后调用 scratch 的清理。
- 并发测试验证每个 Skill 得到独立 `cwd` 且并发执行预算仍为 4。
- `cargo fmt --all -- --check`、`cargo test --workspace --locked --offline --quiet` 与 `cargo clippy --workspace --locked --offline -- -D warnings` 在 Windows 均通过。
