# 1039：Skill 执行前由 Tools owner 复核可执行状态

## 状态

已采纳并实施（2026-10-12）。

## 背景

App command adapter 在授权前检查 `SkillInfo.executable`，但 Skills execution port 后续通过通用 `get_skill` 读取 registry。通用读取也会返回已禁用或不含脚本的 Skill，导致执行 owner 没有把自己维护的启用状态与脚本可用性作为执行前条件。

## 决定

- `SkillRegistry` 提供具名的 executable lookup；该操作同时要求 Skill 存在、当前启用且包含脚本。
- Tools execution port 在副作用前使用该 lookup；不以 App 的早期展示快照作为授权/执行资格依据。
- lookup 后若 Skill 被禁用，已取得的快照可视为已经开始的 in-flight 执行；后续调用必须观察到禁用状态。

## 影响

IPC、Skill 文件和持久数据格式不变，无需数据重置或迁移。禁用或无脚本的 Skill 在 Tools owner 处失败关闭；已开始的运行不被 registry 状态切换追溯取消。

## 验证

- Tools facade 测试覆盖禁用但有脚本的 Skill 不可执行。
- `cargo test --locked -p haven-tools`
- `cargo fmt --all -- --check`、`git diff --check`

## 回滚

若未来支持运行中禁用立即取消，应新增明确的执行 lease/cancellation 生命周期；不得退回到由 App 快照决定执行资格。
