# ADR 1089：约束并净化 Skill 环境准备

## 状态

已接受（2026-10-13）

## 背景

Skill 执行先创建 Python venv 并安装 requirements，再运行脚本。先前总超时只覆盖脚本；直接 App IPC 调用在环境准备卡住时可能长期不返回。setup 命令还用 `wait_with_output` 无界保留 stdout/stderr，失败时把原始 stderr 和 requirements 路径带入错误文本，可能把私有索引凭据或本机路径写入 ToolResult observation。

## 决定

- SkillRunner 的一个 deadline 覆盖 venv 创建、requirements 安装和脚本执行；取消或超时时丢弃 setup 子进程 future，由进程 containment 和 kill-on-drop 清理受管进程树。
- setup stdout/stderr 按流分别最多保留 1 MiB，同时持续 drain，避免 pipe 阻塞与无界内存增长。
- 环境准备失败只返回稳定、限域的错误摘要；不把原始 stderr、绝对 requirements 路径或底层 OS 错误拼入工具观察。
- 用户脚本输出保留既有单独限制与脱敏规则。

## 影响

正常安装和脚本执行语义不变；长期 setup 现在按 Skill 总期限停止，超大 setup 输出不会无限占用内存。失败诊断对模型与 transcript 不再暴露原始安装日志。

## 验证

- Skills 测试验证 setup 输出保留上限、安装失败摘要不含底层诊断以及失败 fingerprint 不落盘。
- SkillRunner 复用同一 deadline；运行 Skills/Tools 测试和严格 Clippy。

## 回滚

不得把 setup 从统一期限中移除，或把原始 pip stderr/requirements 路径重新放入错误文本。
