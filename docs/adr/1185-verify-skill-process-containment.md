# ADR 1185：验证 Skill 取消时的进程树回收

## 状态

已采纳并实施（2026-10-15）。

## 背景

ADR 0513 和 ADR 1184 分别验证了 Windows Job Object 原语及前台 Shell adapter 的进程树回收。SkillRunner 也会启动受管 Python 子进程并在取消时终止直接子进程；需要用 adapter 级测试确认其 Job 生命周期覆盖脚本派生的后代。

## 决定

1. 为 SkillRunner 增加 Windows 取消回归测试。Skill 脚本启动同一 venv 的长驻 Python 子进程并记录 PID；取消 Skill 后，测试必须确认该后代退出。
2. Shell 与 Skill adapter 测试共用仅用于 Windows 单测的进程标记/等待/失败清理 helper。Helper 不进入生产构建或公开 API。
3. 该测试证明 SkillRunner 当前取消路径清理进程树；不证明 Skill 的文件系统或网络访问受到 OS 级隔离。
4. 生产执行行为、配置、IPC 和持久数据均未变化，无需重置。

## 替代方案

- 只测 `ProcessContainment` 平台原语：不足以验证 SkillRunner 是否将 containment 持有到取消清理完成。
- 把独立调用工作目录或环境清理当作文件系统沙箱：拒绝。它们限制默认路径和继承状态，但未建立 OS 文件访问边界。
- 让测试启动真实用户目录或网络资源：拒绝。测试只使用临时目录和本地子进程。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked --offline -p haven-tools descendant_processes -- --nocapture`
- `cargo check --workspace --locked --offline`
- `cargo clippy --workspace --locked --offline -- -D warnings`
- `cargo test --workspace --locked --offline --quiet -- --test-threads=4`
- ADR 索引、crate dependency inventory 与 `git diff --check`。

## 回滚

删除 Skill adapter 回归测试和仅测试 helper，恢复 Shell 测试中的本地进程观察辅助代码，并从本文及路线图移除 Skill 验证记录。无需恢复生产逻辑、配置或持久数据。
