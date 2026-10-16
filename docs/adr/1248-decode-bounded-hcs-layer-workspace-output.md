# ADR 1248：有界解码 HCS writable-layer workspace 输出

## 状态

已实现 legacy HCS layer transport `Files`/Windows backup-stream 布局的私有解码器及合成 fixture 单测。尚未调用 native HCS layer reader，也未接入 provider、service 或 Tools；quota gate 继续 fail closed，未运行真实 HCS 验收。

## 背景

ADR 1245 已定义从最终 workspace 树生成差异并传输到桌面进程的协议，ADR 1246–1247 已把 scratch VHD 绑定为 writable layer，并定义从只读输入 seed 到固定 guest 输出目录的参数。当前缺少从 HCS writable layer 取得可审阅 workspace 树的宿主侧步骤。

微软将 `HcsExportLayer` 描述为生成 layer 的 transport-format 表示；该导出结果不能直接当成普通目录树扫描。Windows layer 的 legacy `Files` 布局使用文件属性前缀和 backup streams，目录元数据可能以 `.$wcidirs$` 文件表达。直接递归复制会把传输元数据暴露为用户文件，或误读目录和文件属性。

## 决定

- 增加 crate-private extractor，只识别 legacy `Files` transport layout，并只接收固定 `Haven\workspace-output` 子树。缺少 `Files` 根或无法解析的记录均拒绝，不猜测其它格式。
- workspace 输出根必须由固定 seed process 成功建立；其内容代表完整输出树。只提取唯一未命名 `BACKUP_DATA` stream；目录标记只有在 backup attributes 确认目录时才转换为目录，合法的同后缀普通文件保留原名。删除项不从 layer 差异重建；被删除的输出项在当前树中不存在，后续由基线比较生成删除差异。
- 输出写入调用方提供的空 `DirectoryCapability`。路径必须保持相对、规范且符合 Windows guest path 规则；拒绝 reparse point、special entry、重复文件/类型冲突、损坏或多重 data stream、深度/条目/路径/单文件/总字节超限和取消。任何错误都可能留下部分树，调用方必须丢弃整个 destination。
- extractor 当前是可单测的格式解码步骤，不持有或调用 HCS API，不证明真实 Windows build 的 layer transport 布局。后续 native reader 必须明确支持格式；检测到其它布局时保持 unavailable，不能回退到 host writable mapping。
- broker quota gate 保持 `quota_unattested`。本切片不执行 guest 命令、不开放 WorkspaceWrite，也不把合成 fixture 当成 HCS 运行期证明。

## 替代方案

- 将 HCS transport 文件夹直接复制为 workspace：拒绝；它包含 layer 元数据和 backup-stream 编码。
- 在容器配置中增加可写 host workspace mapping：拒绝；这会绕过 attempt writable-layer quota 并暴露宿主路径。
- 遇到未知 transport 布局时按空输出继续：拒绝；会把提取失败伪装成用户删除了整个 workspace。

## 影响与迁移

该解码器尚未进入生产调用链，不改变数据库、用户配置、IPC 或持久化数据，无需重置。它只生成 attempt-private 暂存树；review manifest、传输、用户确认和文件晋升仍由既有 owner 负责。native reader 接线前，HCS backend 仍不可执行。

## 验证

- `cargo fmt --package haven-platform -- --check`、`cargo check --locked -p haven-platform`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings` 通过；测试结果 119 passed、1 ignored。
- 单测使用合成 `Files`/backup-stream fixture，覆盖固定子树、目录元数据标记、同后缀普通文件、reparse 属性、损坏 stream、超限、取消、非空 destination 与未知布局拒绝。
- 未运行 HCS layer export、guest quota over-write 或真实 HCS 目标机验收；本地文件系统 fixture 不证明 HCS transport 格式的运行期兼容性。
- 参考：[Microsoft HcsExportLayer](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsexportlayer)、[Microsoft WIN32_STREAM_ID](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-win32_stream_id)。

## 回滚

可移除该 extractor 模块和 ADR；它未接入 provider，因此不影响现有执行行为。native reader 尚未正确识别 layer 格式、不能有界解码或失败后不能丢弃整个 attempt 时，保持 provider unavailable 与 quota gate closed。
