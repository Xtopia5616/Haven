# ADR 1428：以资源授权和统一申请替代默认拒绝

## 状态

已实现，验收结果在本记录的验证节更新。替代 ADR 1404、1408、1416–1423 中“未命中范围直接拒绝”、Git 自动只读和子进程固定离线的授权语义；既有 MXC 隔离、句柄安全与进程树管理仍适用。

## 背景

原授权器把未知权限与用户拒绝混用，默认文件根、域名名单、只读模式以及各执行器的本地检查会先于允许规则拒绝操作。确认界面只支持少量一次性边界扩展，批准后仍可能被另一层拒绝。Shell 自动保护 `.git`，导致正常 Git 工作必须使用另一入口。后台服务没有统一的交互端口。

## 决定

- 可执行请求先检查结构、禁用能力及用户明确配置的硬拒绝；在可申请的资源范围内，精确永久/会话允许优先于 capability 拒绝规则。没有匹配允许时遵守明确拒绝；默认只读读取继续执行，额外文件、网络及外部效果进入确认。合同明确为 Safe 且无需确认的内部交互（如 Ask、通知与 CodeMode 编排）无需为自身再申请权限，其实际资源子调用仍独立授权。Analyze 与 Unattended 都可申请权限。Critical/Required 仍每次生成新确认，但本次已批准的 Critical 收据可以执行。
- 默认根和只读路径是可申请扩展的默认权限。收据及 proof 绑定输入、资源、执行工作区代次、策略 revision、风险和期限。允许保存精确/目录资源授权；撤销或策略变更使旧 proof 失效。无效路径、显式文件拒绝、显式禁止网络、禁用能力及不可落实的隔离条件不能伪装成可执行授权。
- 删除 `NetworkResourcePolicy.allowed_domains`、旧工具 `allowed_domains` 输入与设置编辑器。网络权限通过 `PermissionResourceScope::NetworkDestination` 表达协议、主机和端口。HTTP/MCP HTTP 通过同一交互端口请求；私网和 loopback 同样申请。DNS 先解析元数据，origin 与额外私网地址在一次确认中共同展示；实际连接固定到该答案。HTTP 每个新 origin 再申请，跨源不转发原请求头；保留 POST 正文的跳转额外确认正文披露。未授权的 native HTTP 入口不能借默认 context 绕过检查。
- MXC 不能逐域名约束任意子进程。因此新增 `ProcessNetwork` 资源，绑定实际命令/参数/配置 fingerprint，明确申请公网出站、私网双向通信与宿主 loopback。界面必须显示这个实际范围，不能称其为某个域名的授权。Shell、Git remote、LSP、Skill、Hook 与 MCP stdio 消费同一证明，不再用独立的 Restricted/只读默认拒绝代替询问。没有新的透明代理或内核目标规则，不宣称任意子进程已经实现逐域名批准。
- Hook 与 MCP stdio 的启动请求也经过 `PermissionInteractionPort`，由 AppCommand 的既有交互生命周期呈现。模型调用继续由 Agent owner 呈现；两者共享确认 UI、资源描述、收据校验和授权 owner。Hook 等待用户的时间不消耗五秒进程执行预算；取消、期限和旧快照均阻止启动。MCP 连接尝试的取消会同步取消申请；撤销已保存的连接网络授权会关闭现有 stdio/HTTP generation，普通工具调用授权变化不触发连接级关闭。
- 已配置的 SDK 与运行时读取根是既有只读授权，不再被默认项目路径根抹去；显式文件拒绝仍执行。普通 Shell、Files 和 Skill 不再自动给 `.git` 等版本控制目录加只读规则，用户仍可明确配置保护。
- Everything 在 ReadOnly 下可通过只读项目范围和私有 scratch 运行。FullAccess 下不可约束的网络范围必须申请，而不因 Restricted 默认直接拒绝。

## 容量与清理

Shell 维持每调用 scratch 256 MiB、npm 缓存 512 MiB、Cargo home 2 GiB、最多八个并发 scratch；运行中监控，超限终止，缓存只在空闲时重置，否则由后续空闲或重启清理。输出日志仍按七天、128 个和总计 32 MiB 保留。

LSP/Git/MCP stdio 每个 managed scratch 为 256 MiB，Everything 为 64 MiB，单目录最多 100,000 条目；这些受管进程 scratch 合计最多 16 个。MCP 和 Everything 增加运行中容量检查。scratch 持有 owner 文件锁，正常析构删除，重启清理跳过活跃锁并回收遗留目录。目录计量下沉 Platform 单一实现，保留不跟随重解析点及扫描预算。容量是轮询约束，不能当作操作系统硬磁盘配额。

运行时确认最多 32 个在交互中等待，AppCommand 队列最多 128 条；取消/过期会清除 pending 并投递终态。HTTP 单次目标缓存由跳转上限约束。MCP 容量回归测试复用工作区已有的 test-only `tempfile` 依赖。

## 职责复核

现有 security engine、execution、MCP manager 与 HTTP 文件超过约 800 行，仍各自保留授权判定、执行许可、连接生命周期和 HTTP 执行的单一 owner，大部分新增验收沿用同文件测试。运行中权限申请新增独立 consent 模块；目录计量下沉 Platform 独立 runtime_budget 模块。本次不混入无关的机械拆分。

## 替代方案

保留白名单再增加绕过开关会保留双重判定；把 Restricted 映射为 Open 会在用户批准前扩大权限；假装代理环境变量能强制任意子进程逐域名访问会产生绕过。均不采用。无用户批准时不会回退宿主进程。

## 验证

覆盖授权优先级、精确资源保存/撤销、私网及回环、拒绝与取消、Critical 新确认、HTTP 跨域重新申请及移除原请求头、拒绝后不建立连接、MCP 连接授权撤销、MCP scratch 超限终止、目录活跃锁及退出清理。

2026-10-26 在 Windows、Rust 1.99.0、Node 24.21.0、pnpm 12.12.1 下完成：

- `cargo fmt --all -- --check`、`cargo check --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings` 通过。
- `cargo test --workspace --locked` 通过，最终全量使用 `RUST_TEST_THREADS=4`，没有过滤测试；Agent 689、App 277、Common 296、LLM 561、MCP 122、Tools 1013 项通过，MCP 集成 7 项通过，其他 workspace crate 与文档测试通过。保留既有 ignored 用例。默认并发首轮出现 Skill 清理用例失败，隔离复跑及四线程全量通过，未放宽断言或超时；真实 Windows MXC Hook fixture 同时通过。
- UI 类型检查零错误/警告，170 个测试文件、1519 项测试通过（`--maxWorkers=2`），生产构建及 CSP 生成通过。
- IPC 契约生成/漂移、事件目录、内部 crate 依赖、ADR 索引和 staged diff 检查通过。

真实 Windows 服务与全部第三方 LSP/Skill/MCP/Everything 的端到端覆盖继续按发布 Gate 记录，不把策略及受控 fixture 测试当成全部实际程序通过。已有的 Python runtime 选择改动单独保留，本切片不提交。

## 回滚与重置

数据库 schema 与 session event 格式不变，无需数据库重置。新增资源枚举沿既有 JSON grant 存储；回退到旧二进制前需清除新 `process_network` 授权。升级前删除配置中的所有 `allowed_domains` 项（包括空数组），或按 release-and-reset 重建配置；不把旧名单自动转换为授权。已有 allow/deny 使用新的明确授权优先级，升级时应复核授权列表。任何重置由用户执行，本改动不自动删除用户配置、会话或数据。
