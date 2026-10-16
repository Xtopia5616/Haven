# ADR 1267：将 Skill 输入与 Python launcher 接入 HCS broker

## 状态

已采纳并实现平台侧 broker 链路。实时授权 adapter 尚未接入；`WorkspaceWrite` 的 Skill 工具仍 fail closed。真实 HCS 与超额写入验收保持为独立发布 Gate。

## 背景

ADR 1265 定义固定的无网络、stdlib-only Python Skill profile；ADR 1266 固定 pinned image 内的 Python 版本与 executable digest。只有 contract、profile 检查还不足以执行 Skill：broker 还需要独立传入只读 Skill bundle 和 JSON 参数，并把 guest 输出严格限制到现有 ExecutionWorkspace review 流程。

## 决定

- named-pipe 协议升至 v7。Skill bundle 使用独立 control message 与二进制 frame 通道，限制为 1,024 entries、深度 16、单文件 16 MiB、总量 64 MiB；参数使用独立数据 frame，限制为 64 KiB，并校验长度、SHA-256 和 JSON object。control request 只携带参数长度与 digest。
- 客户端先把经 capability 读取的 Skill bundle 复制到临时私有 capability，再传输并清理该临时副本。broker 在创建 HCS 前校验 entry script 存在、拒绝 `requirements.txt`、将参数写入 attempt-private 输入目录；任何传输或校验错误都清理该 attempt 并 fail closed。
- HCS guest 使用固定只读映射 `C:\Haven\skill-input`，将 bundle 与 `parameters.json` 同时和可写 workspace 隔离。Skill 只能通过 workspace snapshot 修改项目文件；只有 workspace writable layer 的有界差异会进入已有 `SandboxOutputReview`，Skill 输入不会参与输出晋升。
- 固定 Python executable 使用 `-I -S -B` 启动内嵌固定 launcher。launcher 只把 bundle 所在目录加入模块搜索路径，将参数 JSON 接到 stdin，使用固定 entry script；宿主路径、解释器路径、环境变量和参数正文都不由请求选择或放进命令行。
- Skill 请求在 attempt 和文件输入处理前必须同时通过已 attested quota 与独立 Python runtime gate。guest 执行仍沿用 broker deadline、取消 watcher、有界 stdout/stderr、启动授权握手、显式停止和 writable-layer review。
- 平台 port 已提供与 Shell 相同的进程创建前回调、开始确认、取消和输出边界；Tools 尚未把 Skill 的实时授权 proof/revocation 接到该 port，因此此代码路径不授权 `skill__*` 工具，也不改变现有 fail-closed 策略。

## 替代方案

- 把参数放进命令行或 control JSON：会扩大日志/诊断泄漏面，且不适合有界二进制输入，拒绝。
- 将 Skill bundle 合并到 workspace：会让代码成为可写输出的一部分，也会混淆只读来源和用户项目数据，拒绝。
- 直接在 HCS guest 挂载宿主 Skill 根：会暴露宿主路径和可变源目录，破坏快照身份，拒绝。
- 在 guest 内运行 pip 或开放网络：不符合固定离线依赖 profile，拒绝。

## 影响与回滚

该切片只增加平台 broker wire/runtime 行为，不改变数据库、Skill 配置或持久数据。broker client/service 必须作为同一版本部署，因为协议 v7 不保留旧 wire 兼容。回滚时关闭新的 Skill target/映射与 launcher，并恢复此前协议版本；已有 workspace 与 Skills 数据无需迁移。

## 验证

- `cargo fmt --all`、`cargo check --locked -p haven-platform`、`cargo test --locked -p haven-platform`（181 passed，1 ignored）和 `cargo clippy --locked -p haven-platform -- -D warnings` 通过。
- 单测覆盖参数长度/digest/JSON、bundle 独立 frame 通道、只读 HCS 映射、固定 Python/launcher 路径、参数不进入命令行，以及 broker 输出仍绑定 workspace review。
- 本切片未运行真实 HCS、guest Python 或超额写入验收；该验收需单独配置 Windows Containers/Hyper-V、镜像和受限 broker 环境。
