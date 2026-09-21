# ADR 0198：稳定核心工具面与统一 builtin 目录加载

## 状态

已接受（2026-09-21）。

## 背景

渐进式能力加载已经把大部分 builtin operation 移入 deferred catalog，但 provider
surface 仍需要一组稳定入口。此前 builtin 的发现与加载分别暴露为 `tool_catalog` 和
`load_builtin`，模型必须在详情查询后切换工具，核心面也会因 Skill/MCP 配置变化而波动。

## 决定

1. 始终注册一小组稳定核心工具：`ask`、`notify`、`tool_catalog`、`load_skill`、
   `load_mcp`，以及少量高频的文件/系统读取 operation。可选 Skill/MCP 未配置时，loader
   仍存在并返回明确的不可用结果；用户显式禁用工具的 `ToolConfig` 语义不变。
2. 删除独立的 `load_builtin` provider 工具。`tool_catalog` 统一支持 `list`、`describe`
   和 `load` 三类 action；`load` 仅接受 builtin 的 `operations` 或 `roots`，沿用原来的
   session 原子预算检查，成功后下一轮才把 schema 放入 provider `tools[]`。
3. `tool_catalog` 的 `list`/`describe` 保持无副作用；Skill 与 MCP 继续分别由
   `load_skill`/`load_mcp` 激活。恢复逻辑只重放 `tool_catalog(action=load)` 事件。

## 影响

模型每轮看到的控制面稳定，builtin 发现、精确 schema 查询和激活可以沿同一工具契约完成，
减少一次工具切换和一套重复 schema。provider 工具预算、权限、安全矩阵和 deferred/session
隔离边界不变；旧的 `load_builtin` 进行中快照不再兼容。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-tools`
- `cargo test --locked -p haven-agent`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚与重置

代码回滚即可恢复旧入口。若测试版会话快照仍包含 `load_builtin` action，应结束会话或按发布
策略重置进行中的 snapshot；不添加永久兼容分支。
