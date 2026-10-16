# ADR 1265：定义 brokered Skill 执行契约

## 状态

已采纳；平台 typed contract 已实现，HCS broker 执行接线仍待后续切片。`WorkspaceWrite` 中的 Skill execution 继续 fail closed。

## 背景

ADR 1264 将 `load_skill` 激活与 `skill__*` Python 执行分开。HCS 现有调用链只接受 Shell 命令和一个 workspace snapshot；它没有独立 Skill bundle、参数数据通道或 guest runtime / dependency profile。将 Skill 包装成 Shell 字符串无法表达这些输入，也无法证明网络与文件访问边界。

## 决定

- 第一版 Skill guest profile 固定为 `haven-python-skill-v1`。调用者不能选择解释器路径或版本；管理员 pinned image 必须包含并验证该 profile 对应的精确 Python 构建后，broker 才能执行。
- 第一版仅保证 guest 预装的 Python 标准库；bundle 根目录有 `requirements.txt` 时拒绝复制，也不得退回宿主 venv/pip 路径。随 Skill bundle 提供的其它代码仍是被隔离执行的 Skill 内容，不构成可信或已验证的依赖。
- guest 网络 capability 固定关闭。参数作为 JSON object 数据传递，最大 64 KiB；entry script 必须是 bundle 内规范相对 `.py` 路径。参数不能拼入命令行，也不能携带宿主绝对路径。
- Skill bundle 作为独立只读 `DirectoryCapability` 输入，经文件数、深度、单文件和总大小限制复制到 broker 私有 staging；broker IPC 不得携带宿主路径。Skill 对项目文件的访问只能发生在独立 ExecutionWorkspace snapshot 中；所有 guest 文件变化使用现有 `SandboxOutputReview` manifest、预算和晋升授权。
- 每次执行沿用现有授权、策略 revision、HCS attempt quota、deadline、取消及有界输出规则。bundle 或参数验证失败、profile 未 attested、网络/依赖边界未实现时，必须在 attempt 开始前拒绝。
- `haven-platform` 新增仅表达上述固定 profile 的 `WindowsContainerSkillExecutionContract` 与隐藏根路径的 `WindowsContainerSkillBundleInput`。本 ADR 当前完成请求验证和文件输入 capability；它不代表 broker 已支持 Python，也不改变工具授权。接入 guest launcher、参数数据传输和 runtime attestation 前，`WorkspaceWrite` 仍拒绝 Skill adapter。

## 替代方案

- 直接把 Skill 脚本和参数编码进 Shell 命令：会混合代码与数据、暴露命令解析面，且不提供固定 Python、Skill bundle 或依赖闭包，拒绝。
- 在 guest 中运行 `pip install`：将索引、解析、下载和动态依赖引入执行链，破坏离线依赖闭包和网络关闭策略，拒绝。
- 继续使用宿主 venv 并仅限制 `cwd`：不能约束用户文件或网络访问，不能作为 `WorkspaceWrite` sandbox，拒绝。
- 自动把完整 Skill 目录挂为可写 workspace：让 guest 能修改其后续执行的代码与依赖，破坏只读输入和可审阅输出边界，拒绝。

## 影响与回滚

该契约仅增加平台运行时类型，不改变配置、数据库、IPC 或 Skill 持久格式，无需数据重置。尚未接通的 HCS 调用链和 `WorkspaceWrite` 拒绝行为不变。回滚时删除新 contract/input 类型和本 ADR；已有 Skill、workspace 与 broker 数据不受影响。

## 验证

- 平台单元测试覆盖固定 profile、参数对象与 64 KiB 上限、entry path traversal/宿主路径/Windows 保留名拒绝以及 Debug 参数脱敏。
- `cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings`、ADR 索引与 crate 依赖检查按本切片执行。
- 本切片不运行真实 HCS、Python guest 或超额写入验收；Windows broker 接线与 image runtime attestation 仍是后续实现和发布 Gate。
