# ADR 1182：限制 MCP stdio 的继承环境变量

## 状态

已采纳并实施。

## 背景

MCP stdio server 是用户配置的外部进程，之前直接继承 Haven 主进程的完整环境，再叠加该 server 的运行时环境项。主进程环境可能包含 provider token、代理凭据、Python/PIP 路径覆盖或其它与该 MCP server 无关的值。Skill venv 准备进程已经采用最小运行环境白名单，但 MCP stdio 尚未复用该策略。

## 决定

- MCP stdio spawn 前清空环境，仅恢复运行时基线 `PATH`、`SYSTEMROOT`、`TEMP` 和 `TMP`；Windows 另恢复命令解析所需的 `COMSPEC` 与 `PATHEXT`。
- 在基线上设置 `MCP_AUTOLAUNCH=0`；当前 server profile 显式提供的 `env`（包括从 Credential Manager 解析的值）最后写入，作为唯一的 server 专属覆盖和 GUI launch opt-in。
- Skill venv 与 pip 准备进程复用相同的基线过滤器；其 `PYTHONIOENCODING` / `PYTHONUTF8` 固定值仍由 Skill bootstrap 自己追加。通用 Shell 保留交互式用户环境契约，本决定不改变 Shell 命令环境。
- 依赖隐式父进程环境的 MCP server 不再自动收到这些变量；需要的值必须放在该 server 的显式运行时环境配置或安全凭据引用中。
- 环境白名单只减少无关变量的直接传递，不限制子进程以当前 Windows 用户身份读取文件、启动后代或访问网络；进程树回收和 workspace 授权仍是不同边界。

## 替代方案

- 继续继承完整环境：拒绝，host 级凭据和代理配置会无意进入每个外部 MCP server。
- 仅删除已知 token 名称：拒绝，新的 provider、代理或运行时变量容易遗漏，明确白名单更可审计。
- 清空环境后禁止任何 profile 环境项：拒绝，server 需要的认证值应由用户选择性地配置，而不是依赖主进程环境。

## 影响与迁移

依赖 `HOME` / `USERPROFILE`、代理环境或其它未列入基线变量的 MCP server，需要将所需值显式配置在该 server profile 中。安全存储引用继续只注入其绑定的环境变量名和值。该变更不增加持久字段或数据库迁移；环境配置仍按现有运行时/凭据契约处理。

## 验证

- `haven-platform` 的纯过滤测试确认保留最小基线并拒绝 provider token、代理、Python/PIP 覆盖变量。
- MCP stdio 子进程验证确认父进程哨兵变量不会继承、显式 profile 值与 `MCP_AUTOLAUNCH` opt-in 仍生效。
- 运行 workspace Rust 测试、严格 Clippy、格式检查和 `git diff --check`。
