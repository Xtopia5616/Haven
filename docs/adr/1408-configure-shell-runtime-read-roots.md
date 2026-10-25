# ADR 1408：配置 Shell 系统与工具链只读根

## 状态

已采纳并实施。

## 背景

WorkspaceWrite 下的 Shell 由 MXC 限制文件访问。其基础只读根包括 Windows 运行时目录和实际子进程 `PATH` 目录；`PATH` 常只指向可执行文件入口，SDK、解释器标准库和其它依赖目录未必在其中。第一次运行因此可能因只读依赖被拒绝，随后只能依赖一次性拒绝记录确认重试。

## 决定

1. `SecurityConfig.shell_runtime_read_roots` 是 WorkspaceWrite Shell 的显式、持久附加读取目录清单。设置页说明 Windows 运行时和 `PATH` 入口会自动纳入，并允许用户列出 SDK、解释器或库目录；最多 64 个路径。新配置缺少该字段时默认为空，保留当前自动根行为。
2. `AuthorizationEngine` 是最终 MXC 路径策略的唯一编译者。Shell 适配器只从传给实际子进程的过滤后环境提取 Windows 运行时和 `PATH` 目录，并把它们作为候选读取根交给授权 owner；适配器不再自行追加只读根。
3. 自动候选根与配置根都按只读能力处理，并逐一与全局及 Shell 工具级 `path_roots` 求交。全局/工具级 `denied_paths` 始终优先；嵌套在可写根内的系统/工具链目录仍保持只读。无路径交集的候选根被排除；cwd 无法落入最终根时继续拒绝启动。
4. 结构化 MXC 拒绝记录仍只可发起当前调用的一次性扩展。它不能覆盖全局或工具级路径限制、deny、工具只读配置或 Analyze/Unattended 边界；反复失败不会自动写入持久策略。用户要持续读取某个 SDK 时，应把该目录加入上述显式设置。
5. 所有新增路径必须是可安全打开的现存本地目录。配置变化推进安全策略版本并撤销受影响的进程；没有可用目录或策略无法落实时 fail closed。

## 替代方案

- 继续在 Shell 适配器里追加环境路径：拒绝，会让进程适配器绕过授权 owner 对全局/工具路径与拒绝规则的编译。
- 根据拒绝记录自动保存新根或重复重试：拒绝，失败输出不是持久授权；自动放行会把模型命令决定的依赖变成后续调用权限。
- 默认递归开放整个 Windows、Program Files、用户配置目录或所有 SDK：拒绝，无法解释实际读取集合，也会显著扩大可读范围。
- 只依靠 `PATH`：拒绝，它通常只定位可执行文件，不代表 SDK 或解释器库目录。

## 影响与回滚

只新增可选配置字段和 Settings IPC 字段，不改数据库、session event 或 MXC wire shape；不要求数据库重置。缺少字段的配置保持原有行为。旧二进制不认识新增的 `security` 字段，回滚到旧版前应恢复升级前的 `config.toml` 备份；不要通过删库处理配置兼容问题。

回滚时同时删除设置项、配置字段及授权 owner 的根编译逻辑，并恢复由 Shell 适配器提供 Windows/PATH 根的单一行为。不能让适配器把用户配置根或确认记录重新追加到 MXC 请求。

## 验收

- 配置 round-trip、绝对本地路径校验和 64 根上限有回归覆盖。
- Tools 策略编译覆盖：配置 SDK 根位于全局和工具级路径交集内时可读；路径交集外的根不进入 MXC；全局和工具级 deny 子路径仍进入 MXC deny 列表。
- Settings UI 展示自动系统/PATH 根与用户补充依赖根的区别，并可编辑附加根。
- `cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、UI check/test、IPC 合约检查及 ADR index 检查通过；Windows MXC 真机限制仍以发布 Gate 的当前环境验收为准。
