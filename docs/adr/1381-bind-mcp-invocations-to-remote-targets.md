# ADR 1381：将 MCP 调用绑定到远程目标与精确参数

## 状态

已实施 — 2026-10-23。

## 背景

MCP server 和 tool schema 都是远程提供的数据，通用 adapter 无法从任意参数名推断目标对象、账户或读写语义。此前授权 proof 绑定了当前 manager 的连接代次，但持久授权范围没有对象身份，也不能避免相同配置名称下改换 endpoint/账户后复用旧的 Always grant。

## 决定

1. MCP tool 的授权契约在同一快照中解析操作策略、当前连接 target binding 和 typed resource。target binding 同时包含 manager 身份、连接代次与连接配置的 SHA-256 指纹；配置指纹覆盖 server 名称、transport、stdio 命令/参数/工作目录/环境变量名及 Credential Manager 凭据引用，或 HTTP URL/enabled 状态。manager 身份和连接代次阻止等待确认期间的替换；稳定配置指纹使持久范围对连接配置变化敏感。原始 endpoint、命令和环境值不进入授权资源或 UI，避免凭据值参与可见指纹。
2. MCP resource 是一项远程调用，其稳定身份由稳定连接配置指纹与 canonical JSON 完整参数指纹组成。配置指纹不包含临时 manager 身份或连接代次，因此同一配置重连后可继续匹配持久授权；执行 proof 仍绑定当次连接代次。adapter 不根据非可信 schema 猜测哪个字段代表对象 ID；参数中的目标对象、操作和数据都包含在同一精确请求摘要中。
3. 持久授权只匹配同一 capability、稳定连接配置指纹和完全相同的参数摘要。任何远程 target 或参数变化都要求重新确认；MCP scope 不支持 directory/子树继承。
4. 执行前 adapter 用调用参数和授权 target 重算资源并对比 proof 中的完整资源集，再通过 proof-derived execution authorization。MCP manager 仍按原 target binding 获取该连接代次并路由请求；替换或断连 fail closed。
5. 所有 adapter 的 policy、target binding 和 resources 统一由 `ToolAuthorizationContract` 快照返回，AuthorizationPolicy 不再分开查询，以免配置/连接在两次读取间变化。

## 配置与数据影响

数据库和 MCP wire 参数不变。安全配置的 `PermissionResourceScope` 增加 `mcp_remote_invocation` 判别分支；无旧授权可自动扩大到此范围，无需数据库或配置重置。IPC typed union 与授权 UI 描述同步更新。`haven-mcp` 使用现有 workspace `sha2` 依赖，对本机连接配置生成不暴露明文的稳定指纹。

## 替代方案

- 只按 MCP tool 名永久授权：拒绝；同一远程操作会覆盖参数中的多个用户/记录/对象。
- 根据 MCP JSON Schema 中的 `id`、`path` 等字段名推断对象身份：拒绝；schema 是远程提供的非可信数据，各 server 的语义不一致。
- 将原始参数、URL、命令或 credentials 存入 grant：拒绝；授权只保存摘要，UI 只显示连接与精确参数均已绑定。
- 只绑定连接代次：拒绝；连接实例编号不表达持久的 endpoint/账户配置身份。

## 验收与限制

测试覆盖同一配置指纹稳定、URL/凭据变化导致指纹变化、不同连接或调用参数不能复用授权、参数与 proof 资源不匹配时拒绝执行、持久 scope 指纹校验，以及 direct MCP 和 Code Mode 共用授权请求路径。MCP server 若在连接存续期间自行切换其服务端账户或将同一 endpoint 重新指向另一账户，客户端协议没有可信账户身份可供验证；此情况无法由本地配置摘要检测，用户应在执行前核对服务端身份。真实远端服务的身份仍是 Windows 发布验收与具体集成的信任责任。
