# ADR 1341：扩展 LSP 符号导航

## 状态

已采纳。

## 背景

ADR 1340 增加了按源码位置查询定义、引用和悬停等能力。但 Agent 在不知道文件和精确 UTF-16 列号时难以开始导航；遇到接口或类型时，也缺少直接查询实现和类型声明的操作。逐个文件调用 `document_symbols` 只能帮助局部定位。

## 决定

1. 新增 `lsp.implementation` 和 `lsp.type_definition`，分别调用标准 `textDocument/implementation` 与 `textDocument/typeDefinition` 请求，沿用现有 1-based 行列输入和安全策略。
2. 新增 `lsp.workspace_symbols`，调用 `workspace/symbol`，以已启用的 `lsp_servers[].name` 选择语言服务器，并按非空 `query` 搜索其工作区索引。它不要求用户指定源码文件或坐标，也不打开某个源文档。
3. 对这三个操作沿用 High risk、UserData、Opaque network、HostProcess、NonIdempotent、Full Access 和既有授权确认要求。符号结果中的工作区文件 URI 继续转换为工作区相对路径，工作区外文件 URI 继续脱敏。
4. 增加面向 Agent/开发者的 LSP 配置和调用指南，并保持 `lsp_servers` 配置格式及 Settings IPC 边界不变。
5. 保留每次调用启动短期服务器的生命周期；不引入常驻进程、索引缓存或配置 UI。其取消、授权撤销、进程回收和工作区隔离语义仍由 ADR 1340 定义。

## 考虑过的替代方案

- **Agent 继续从文件内容推算坐标：拒绝。** 对工作区符号搜索而言，文件定位和 UTF-16 列号是本可避免的调用负担。
- **新增 Haven 自有的全工作区符号索引：拒绝。** 这会重复语言服务器的索引职责，并增加语言特定解析器和索引生命周期。
- **把常驻进程池纳入本次改动：暂不采用。** 进程池需要额外定义授权 proof 生命周期、workspace/config generation 隔离、取消、崩溃重启、并发请求和 shutdown drain；先保留 ADR 1340 的短期会话边界。

## 影响与兼容性

- 新增操作不改变持久配置、数据库、Tauri IPC 或现有工具参数；未配置服务器的环境保持原有行为。
- 工作区符号搜索的结果依赖指定服务器是否支持 `workspace/symbol` 以及其索引行为；不支持时返回语言服务器错误。
- 每次查询仍需冷启动语言服务器；这是当前明确的性能限制，不代表支持常驻索引。
- Windows 的真实进程树 containment 验收仍按 ADR 1340 记录的发布验收门执行。

## 验证

- Tools 单元测试覆盖按配置名选择已启用服务器、工作区符号请求参数、实现/类型定义请求方法、operation schema 与安全 metadata。
- 更新 LSP prompt、操作目录契约、架构说明和使用指南。
- 运行受影响 crate 的 locked 检查、严格 Clippy、测试与 workspace 格式检查；无需数据库或配置重置。

## 回滚

移除新增的三个操作、对应 prompt/契约/测试和本文档中的新增指南条目即可。配置格式、数据库及 IPC 均未变化，无需重置用户数据或配置。
