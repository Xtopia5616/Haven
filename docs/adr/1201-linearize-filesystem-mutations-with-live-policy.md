# ADR 1201：在文件副作用期间固定实时授权策略

## 状态

已实现。proof 派生的 `ExecutionPolicyAuthorization` 现用于 Shell 进程创建和 Files 写入/目录创建/复制/移动/删除；Files 操作必须持有与当前 proof 同 revision、同 workspace generation 的策略读租约直至副作用完成。

## 背景

Files 写路径已从授权 proof 派生 `FileWritePolicy` 和目录 capability，但 attempt proof 校验与实际文件变更之间没有同步实时策略更新。若用户在工具开始后切换 SandboxMode、可写根、权限模式、执行 workspace 或工具 `allowed_paths`，旧 attempt 仍可能按已捕获的 capability 写入。Shell 在 OS spawn 点已用读租约关闭同类竞态（ADR 1199），但文件副作用尚未使用这一线性化边界。

## 决定

- 将只用于进程创建的 `ExecutionSpawnAuthorization` / `ExecutionSpawnPermit` 收敛为通用的 `ExecutionPolicyAuthorization` / `ExecutionPolicyPermit`。能力仍由 Tools 授权 owner 从 proof 派生，只绑定授权 owner、proof 的 policy revision、workspace generation 和有效期，不暴露策略配置。
- 在实际文件副作用前获取当前安全配置的读租约。获取时检查 proof 未过期、policy revision 未变化、workspace generation 未变化；任何差异都以结构化 Permission 错误拒绝。
- Files 的写入、建目录、编辑、patch、copy、move 和 delete 持有读租约直至操作返回。现有 `FileWritePolicy` 仍负责以已打开目录 capability 限制目标路径；租约补充实时策略一致性，不替代路径 capability。
- 前台 Shell 持租约覆盖同步 OS spawn；后台 Shell 持租约覆盖同步 OS spawn，不延长到子进程整个生命周期。运行中的进程撤销仍由进程 owner lifecycle 后续定义。
- 有 proof 派生目录策略但没有实时租约的 Files 请求 fail closed。仅测试构造器可创建测试租约，不进入生产构建。
- 配置更新在正在进行的 Files 副作用期间等待；副作用结束后更新才生效。文件操作仍遵循原有字节、取消与超时上限，租约不跨工具 attempt 或子进程生命周期。

## 替代方案

- 仅在 attempt 开始校验 proof：拒绝，配置可在授权与改盘之间撤销或收紧。
- 只检查 revision 后释放锁再写：拒绝，检查与文件副作用之间仍可发生竞态。
- 让 `FileWritePolicy` 成为永久授权：拒绝，它是某一 attempt 的 capability 快照，必须与当前授权 owner 的 revision/generation 同时有效。

## 影响与迁移

只改变进程内授权与副作用同步，不增加配置、IPC、数据库或持久身份，无需重置。策略写入可能等待有界 Files 操作完成。该锁不协调 Haven 之外的进程对目标文件的修改；路径句柄和现有 compare/hash 行为仍负责其定义的文件系统边界。

## 验证

- Tools 测试覆盖 proof 捕获后策略更新会拒绝 Files 写入，且目标文件不创建；存在目录 capability 但没有实时租约时也必须拒绝。
- `AuthorizationEngine` 既有测试覆盖策略更新先于租约时 fail closed、持租约期间配置写入等待、workspace generation 变化使旧上下文失效。
- 本切片运行 Tools 全量测试、严格 Clippy、locked workspace 检查与格式检查。

## 回滚

还原通用租约重命名及 Files 副作用入口的租约获取；Shell 进程创建仍需保留 ADR 1199 定义的旧进程启动 permit。没有持久数据需要恢复。
