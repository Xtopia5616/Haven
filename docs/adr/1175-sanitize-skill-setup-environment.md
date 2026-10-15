# ADR 1175：净化 Skill venv 准备子进程环境

## 状态

已采纳并实施。

## 背景

首次执行 Skill 或其 `requirements.txt` 变化时，`haven-skills` 会启动 Python `venv` 和 `pip install` 子进程。它们经 Job Object/process containment 管理生命周期，但之前默认继承整个 Haven 环境，包含凭据、代理认证、`PIP_*` 安装源选项以及可改变 Python 导入路径的 `PYTHONPATH`/`PYTHONHOME`。Skill 脚本运行时已经使用显式环境白名单，准备阶段却没有同一约束。

进程 containment 只约束进程树生命周期，不提供用户目录、凭据文件或系统资源的文件系统隔离。此次变更降低通过环境变量直接读取秘密的风险，不宣称建立 OS sandbox。

## 决定

- venv 创建和 pip 安装命令在 spawn 前清空环境，仅传入 `PATH`、`SYSTEMROOT`、`TEMP` 和 `TMP`；Windows 另保留可用于命令解析的 `COMSPEC` 与 `PATHEXT`。
- 应用固定加入 `PYTHONIOENCODING=utf-8` 与 `PYTHONUTF8=1`，不接受进程环境覆盖。
- 不继承 `PIP_*`、代理变量、Python 导入/启动覆盖、home/profile 变量或凭据变量。用户通过这些环境变量提供的私有索引或代理配置不再自动生效；当前没有新的 Skill 专属配置入口。
- 规则只应用于 venv/pip 准备命令；不改变 MCP、Shell、Skill 脚本的既有环境策略，也不改变 Skill 网络授权或文件系统边界。

## 替代方案

- 继续完整继承父进程环境：拒绝，准备安装包的 Python 进程无需直接取得 Haven 的令牌或 Python 路径覆盖。
- 只删除已知凭据变量：拒绝，未来新增秘密变量或工具变量时容易漏掉；采用小型 allowlist。
- 将环境净化称为 sandbox：拒绝，子进程仍以当前用户身份运行，可访问其文件和可用系统资源。

## 影响与迁移

该变化只影响新的 venv 创建和依赖安装子进程。配置的 `PIP_*`、`HTTP_PROXY`/`HTTPS_PROXY` 等环境变量不再传入这些进程；需要企业镜像或代理的场景当前应由 pip 自身配置提供。无数据库或持久配置字段变化，无需重置数据。

## 验证

- 用纯函数输入验证系统运行变量保留，凭据、代理、pip 和 Python 路径覆盖变量被过滤。
- 用隔离 child process 验证显式注入的秘密环境变量在 spawn 前已移除；命令参数和工作目录保持原契约。
- 运行 `haven-skills` crate 格式、测试与严格 Clippy，并运行 workspace Rust 门禁。测试不得修改全局环境或访问真实用户目录。
