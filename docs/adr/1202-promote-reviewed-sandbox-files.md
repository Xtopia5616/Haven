# ADR 1202：逐文件晋升已审阅的 sandbox 新文件

## 状态

已实现平台级单文件原语；Tools 对 manifest 选择、用户确认、实时策略租约和目标 capability 的端到端接线仍待实施。

## 背景

ADR 1200 建立了 AppContainer 输出与私有输入基线之间的有界变更清单。清单本身不能写回 `ExecutionWorkspace`，否则进程退出后的整树复制会绕过 Files 的目录授权与用户确认。现有 Files 原子替换允许创建缺失父目录，并可替换同名文件；这不适合作为“用户审核后晋升 sandbox 结果”的默认副作用，因为审核期间目标可能由其它进程创建或修改。

## 决定

- `AppContainerChild` 仅在其 Job Object 进程树确认归零后，才可调用输出晋升。调用必须给出与 sandbox 创建时绑定的 `execution_workspace_generation` 和 `policy_revision`；不匹配即拒绝。
- 当前平台切片只接受 manifest 中 `Created`、`File` 且具有字节数和 SHA-256 的变更。删除、修改、目录、类型变化和特殊文件均不自动晋升；sandbox 文件必须另存为一个新目标。
- 晋升将重新打开 sandbox 输出文件并在有界流式复制期间复核长度和 SHA-256。内容与 manifest 不符、取消或任一 I/O 失败都会丢弃未提交的暂存文件。
- 目标必须由调用方预先解析为 `DirectoryCapability`；目标父目录须已存在，平台原语不会创建目录。提交使用 create-if-absent 原子语义；目标在检查后出现时仍拒绝，且绝不覆盖已有文件。源与目标根重叠、目标路径不安全、目标为链接或特殊文件时拒绝。
- 这是存储原语，不是授权或用户确认凭证。后续 Tools 接线必须只使用当前 `FileWritePolicy` 解析的 capability，持有 proof 绑定的 `ExecutionPolicyAuthorization` 读租约覆盖晋升，并将当前策略 revision、workspace generation、用户选中的 manifest 路径及新目标与确认 proof 精确绑定。未完成该接线前，不暴露产品 UI/工具调用。
- 收紧 `AppContainerChild::workspace()` 与 `AppContainerWorkspace::directory()` 为 crate 内可见，避免外部调用方绕过“进程树归零后才读取或晋升输出”的 owner 生命周期。

## 替代方案

- 退出后把整个 sandbox 树复制回 canonical workspace：拒绝，会绕过逐路径审阅、`writable_roots`、工具 `allowed_paths` 与当前执行 workspace capability。
- 覆盖同路径的现有文件：暂缓。常规的“先 hash 再 replace”存在检查到替换之间的并发竞态；首版要求选择一个不存在的目标，并用原子 create-if-absent 保证不会覆盖审核期间出现的文件。
- 允许晋升 sandbox 修改或删除：暂缓。覆盖和删除需要独立、可取消、proof 精确绑定的提交语义，不能复用新文件创建原语。
- 在 sandbox 仍运行时读取或晋升：拒绝，后代进程可能在审阅和复制期间继续改写文件。

## 影响与迁移

平台新增有界流式校验和单文件原子创建接口，没有数据库、配置、IPC 或持久身份变化，无需重置。现有 `atomic_replace` 和 Files 的父目录创建语义保持不变。当前能力只供后续授权 owner 集成，产品 Shell 仍不切换至 AppContainer；WorkspaceWrite 对 opaque Shell 继续 fail closed。

## 验证

- 平台单测覆盖创建成功、目标已存在时拒绝且原文件不变、父目录缺失时拒绝、不接受修改项、manifest 后源文件变化、复制取消和不产生部分目标。
- Windows AppContainer probe 覆盖未回收进程树时拒绝、回收后晋升、重复目标拒绝、workspace generation 不匹配拒绝。
- 验收运行 `cargo test --locked -p haven-platform`、workspace check/严格 Clippy、格式、ADR 索引及 crate dependency inventory。Tools 的 proof/确认/UI 垂直接线属于后续切片。

## 回滚

移除 AppContainer 单文件晋升入口和 create-if-absent 原语，再撤回本文、ADR 索引及路线图更新。未改变持久数据，也未向产品入口开放晋升，无需数据恢复。
