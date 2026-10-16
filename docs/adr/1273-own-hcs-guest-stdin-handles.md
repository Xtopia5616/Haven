# ADR 1273：拥有并校验 HCS guest stdin handle

## 状态

已实现 HCS process-info stdin handle 的可选所有权与严格校验。尚未实现 stdin 字节写入、stdout/stderr 异步 session stream 或 MCP broker transport；`WorkspaceWrite` 下 stdio MCP 继续 fail closed。

## 背景

ADR 1270 为 MCP transport 抽取了 `McpStdioProcess` 双向字节流端口，ADR 1269 要求 HCS broker 为一个长生命周期 MCP generation 提供受限 stdin/stdout 字节流。HCS API 的 `HCS_PROCESS_INFORMATION` 含有可选 `StdInput`，但当前 `HcsGuestProcessOwner` 将任何非空 stdin handle 当作异常。继续实现 transport 前，平台进程 owner 必须按固定 `ProcessParameters.CreateStdInPipe` 声明校验并管理该 handle。

## 决定

- `HcsGuestProcessParameters` 暴露只读的 stdin-pipe intent。`HcsGuestProcessOwner::create` 将其与 HCS 返回的 process-info 比较：请求 stdin pipe 时必须返回非空 handle；未请求时必须返回空 handle。stdout/stderr 和 process handle 仍必须完整。
- handle 形状不匹配时，先关闭 HCS 返回的全部 stdin/stdout/stderr handle，再终止并关闭 guest process，随后返回静态 `InvalidData` 错误；不能把不符合请求的半初始化 process 交给上层。
- 正常创建时，stdin 与 stdout/stderr 一样由唯一的 `OwnedGuestHandle` 负责关闭。stdin 可被未来的受限 stream adapter 一次性转移；未转移的 handle 在 owner drop 时关闭。当前 Shell、Skill 与 quota probe 的 `CreateStdInPipe` 都为 false，执行行为不变。
- 本切片不提供同步或异步写接口。HCS stdin 写入的背压、取消、关闭顺序和长 session 生命周期由独立 stream adapter 设计与测试，不从当前 one-shot execution handler 暗中复用。

## 替代方案

- 忽略请求/返回 pipe 形状并继续接受部分 HCS process info：拒绝，后续 transport 可能拿不到可用 stdin 或遗留未关闭句柄。
- 由各调用点分别关闭 HCS raw handle：拒绝，会分散唯一所有权并增加 double-close / leak 路径。
- 把 stdout poller 扩展成通用 stdin/stdout 对象：拒绝，读取、写入和取消的节流模型不同，应由专用 adapter 持有 handle。

## 影响与回滚

仅调整 `haven-platform` 内部 HCS process owner，不改 broker wire protocol、MCP 配置、Tools authorization、数据库或 UI。回滚时移除 stdin ownership branch 并继续要求 HCS 返回空 stdin；现有 one-shot process contract 不变。

## 验证

- 定向单测覆盖按请求接受 stdin、单次转移和关闭、缺失/意外 handle 的 fail-closed 清理顺序。
- HCS API 的 `HCS_PROCESS_INFORMATION` 将 `StdInput` 定义为“如创建则返回”；参考 [Microsoft HCS_PROCESS_INFORMATION](https://learn.microsoft.com/virtualization/api/hcs/Reference/HCS_PROCESS_INFORMATION)。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-platform`（193 passed、1 ignored）、`cargo clippy --locked -p haven-platform -- -D warnings`、`pwsh -NoProfile -File scripts/check-adr-index.ps1`。真实 HCS stdin pipe 和运行中 MCP package 尚未启动。
