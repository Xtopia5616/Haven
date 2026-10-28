# ADR 1449：Files 将操作绑定到解析后的本地目标

## 状态

已采纳；只读 Files 链路部分实现，完整实现与验收仍未完成。

## 背景与威胁模型

变更前，Files 与 `FileReadPolicy` 在路径解析、策略检查和实际打开中拒绝所有 reparse point。工作区原型只做了部分目标解析，尚未证明组件锁定、授权 receipt、proof、实际 Files 操作、UI 与网络边界。此 ADR 收窄第一实现切片为 Windows 本地 NTFS 上存在目标的只读 Files 操作；写入语义、缺失链接目标和网络共享仍是明确拒绝范围。

威胁包括并发替换路径分量或 reparse point、把链接重定向到工作区外/拒绝/只读目标、把本地链接指向映射网络驱动器或 UNC、借一个通过检查的数组项放行其它资源，以及在确认等待、重启或重试后复用过期授权。路径预检查和普通进程 cwd 都不是操作系统隔离边界。

## 决定

### 资源和解析范围

1. 本地链接解析目标与网络共享是两类不同资源。本 ADR 只定义 Files 的本地目标能力；UNC、映射网络驱动器、指向网络的链接、`\\.\` 设备命名空间、无法分类的 drive mapping 和网络文件系统均在任何目标打开前拒绝。授权本地路径不代表网络权限。
2. Windows 仅允许解析 `IO_REPARSE_TAG_SYMLINK` 与 `IO_REPARSE_TAG_MOUNT_POINT`。`MOUNT_POINT` 只接受可验证为本地 NTFS 目录的盘符绝对目标；volume GUID mount、未知 reparse tag、损坏数据、循环、32 层以上链接、128 个以上持有句柄、超出路径/条目预算或任何身份查询失败均拒绝。非 NTFS 或身份语义未验证的文件系统不获得链接支持。非 Windows 平台继续拒绝链接。
3. 解析器逐组件处理 `.`、`..` 和相对链接目标，并对每个路径分量持有 no-follow handle；句柄共享模式拒绝并发写入、重命名或删除，读取链接 tag 与目标期间保持这些句柄。目标为 UNC/device/网络路径时在打开目标前拒绝。随后打开实际目标，比较卷身份、64 位文件 ID、创建时间和对象类型；比较只在持有句柄的当前绑定生命周期内有效，不宣称跨卷迁移、网络服务器或重启后身份不变。该路径检查不是 OS 进程隔离。
4. 目前只解析现存的链接目标。链接目标缺失、链接后续路径分量缺失或需要创建多层父目录时明确拒绝；不得将其降格为“普通缺失路径”。不含链接的新目标继续使用既有的父目录 capability 和相对 suffix 规则。后续如要支持链接后的缺失目标，必须先实现并验收持有现存父目录与 suffix 的 capability。仅从当前只读目标句柄派生 `DirectoryCapability` 不满足该条件：Windows 原子创建提交阶段需要相对目标目录打开带 `FILE_ADD_FILE` / `FILE_DELETE_CHILD` 的句柄，而只读解析句柄的共享模式会使该打开因共享冲突失败。不得通过放宽现有句柄共享模式解决；需要单独设计能固定已解析目录身份、允许目录项写入但仍阻止该目录被替换的 capability，并以真实 Windows 竞态测试验证。

### 操作对象和策略

5. 当前仅 Files `read`、`inspect`、`list`、`outline`、内部 `search` 和文本 `summary` 可跟随已存在的最终或中间本地链接，并消费已绑定的目标句柄/目录 capability。receipt 分别标记精确对象、目录直接项或目录子树范围；目录枚举不跟随目录内发现的链接。Everything 外部程序路径、managed-media/富媒体 handoff 和其它外部调用不接受链接。链接目标是媒体文件时明确拒绝。`write`、`create_dir`、`patch`、`copy`、`move`、`delete` 只要任一资源路径经过链接或 reparse point 就在授权前拒绝；当前不为删除/移动最终链接提供特殊语义。这比目标方案更窄，未实现的变更语义不能按本 ADR 标记支持。
   链接文件的 `write`/`patch` 暂不开放以保留现有原子替换语义：执行 proof 持有目标句柄以防对象被替换，而 Windows 按目录句柄提交的原子替换需要改动目标目录项；放宽目标句柄的 delete sharing 后，又无法把最终重命名原子地条件化为“目录项仍指向已批准的 file identity”。改为直接写句柄会失去原子性，失败时可能留下部分内容，故当前按用户选择继续拒绝。只有找到并验证同时绑定目标身份与原子提交的能力后，才重新评估支持范围。
6. Shell、Skill、MCP、LSP、剪贴板、工作区切换、managed-media 与清理器继续按各自契约拒绝或隔离 reparse point；Files 支持不扩展这些入口的保证。
7. 所有路径边界逐个检查原始请求路径、每个被遍历的 link entry 和最终目标：硬拒绝优先于只读保护，后者优先于全局文件根、工作区根、工具级路径/写策略和普通授权。授权范围必须同时覆盖请求 alias 与实际目标；一次性资源扩展也不能绕过硬拒绝或只读保护。多资源请求全部通过才可执行。
8. 已支持的链接读取在 proof 阶段重新绑定身份，并由 `FileReadPolicy` 消费同一解析目标句柄，目录读取使用从目标句柄派生的 capability。链接写入尚未实现并在授权前拒绝。无法保持批准的目标身份或无法在授权 capability 下消费目标时返回明确权限拒绝；不得回退到按原路径重新打开。

### 确认、持久授权与恢复

9. Trusted resource declaration 保留用户输入路径、解析后的实际目标、访问方向、操作语义、link chain 和目标身份。前端/模型不能提交解析结果；AuthorizationEngine 在授权、receipt 验证、执行 proof 和每次 retry 前自行重解析并逐字段比较。
10. 等待用户确认时不保留 OS 句柄；receipt 只保存有时效的解析摘要与身份，不是能力句柄。用户批准后，AuthorizationEngine 重新解析、比较摘要并创建本次执行 capability。路径、目标、link chain、操作、输入或 policy revision 任一变化均拒绝旧 receipt。进程重启后不能把序列化路径/身份当作仍有效句柄；恢复时重新解析并重新确认，缺少可信新确认则拒绝执行。超时、取消、拒绝、关闭与策略更新不留下 OS 句柄。
11. 含链接路径每次都要求用户确认，即使普通路径或工具授权原本可自动放行；确认只提供本次批准。当前 NTFS file ID 不是跨重启、删除重建的永久身份，故不把链接授权写入 Session/Always grant。UI 隐藏不可安全复用的期限选项并显示用户请求路径、实际目标、访问方向、精确对象/目录直接项/目录子树范围和操作对象。无链接的现有 resource-scoped grant 保持原契约。
12. 网络共享授权是独立后续设计：需先界定 server/share/内路径和读写方向、别名/DFS/重连身份及首次连接前授权。此能力完成前，UNC、映射盘及网络跳转继续在连接/打开前拒绝，不能显示为已支持。

### 网络共享拒绝边界

- 共享资源若进入后续设计，身份至少由用户可辨认的服务器名、共享名、共享内相对路径和读/写方向组成；DNS 别名、DFS referral、映射盘别名、断线重连和服务器/共享替换是否可稳定绑定仍需专门验证。当前没有可证明这些身份稳定的持久授权格式，因此不提供单次、会话或永久共享授权选项。
- 在本地目标解析和普通 Files 打开前，只用本机 DOS-device namespace 对盘符做分类；UNC、映射到 MUP/网络 redirector 的盘符、指向共享的 link target、设备 namespace、volume GUID mount 与无法识别的映射都拒绝。不会查询远程服务器、触发 DFS referral、读取网络凭据或为获得身份先连接共享。DNS/NetBIOS 名称解析不在受支持流程内。
- Windows 自身 ACL/凭据仍按当前进程令牌执行；Haven 的本地授权不改变 Windows 权限，也不等于授予网络访问。超时、断线重连、凭据变化和共享映射变化当前都按拒绝处理；具体共享并发/取消方案需在未来独立 ADR 设计，不在此 ADR 声称支持。
- 扩展长度 namespace、设备 namespace、UNC、映射盘和 link jump 各自分类；目前扩展路径统一拒绝，即使它拼写成本机盘符形式。设备路径不能因未来支持 UNC 而自动放行。

## 替代方案

- 将路径 canonicalize 后继续按路径操作：拒绝，检查和使用间仍可被替换。
- 全面拒绝所有链接：足够保守但不能满足明确的 Files 本地目标场景；仅保留为能力不足或边界不明时的拒绝结果。
- 让 Files 支持后放宽 Shell/外部程序：拒绝，普通进程路径访问不受 Files 句柄约束。
- 将网络共享当作普通本地目标：拒绝，身份解析可能先触发未授权网络访问，映射、DFS 和重连也改变身份边界。
- 将一次链接授权持久化为目标路径：拒绝，路径与 NTFS 文件 ID 无法构成可靠的跨重启对象能力。

## 持久化、重置和回滚

实现只扩展已有 confirmation receipt / typed resource event 的 JSON payload，不增加数据库表或 schema 版本。旧 pending receipt 不含目标绑定时按失效处理并要求重新授权；旧 Session/Always path grant 不自动扩大为 link grant。配置和数据库无需重置。回滚需同时撤销 link-aware Files resource、handle-bound resolver、UI 显示和回归矩阵修改；旧策略仍会拒绝 link 路径。

## 当前实现边界

- Windows NTFS 上存在目标的 symlink/junction 可用于上述只读 Files 操作；确认资源同时携带请求路径、目标路径、访问方向、link chain、对象身份和进程随机 epoch。确认等待期间不持有 OS handle；确认后、执行 proof 捕获时重新解析；epoch、目标身份或 link chain 不匹配就拒绝。含链接资源不能存成 Session/Always grant。
- 网络共享、映射盘、设备路径、未知 tag、无法分类的 drive mapping，以及任何 mutating Files 链接操作均拒绝。链接后的缺失目标也拒绝；普通非链接缺失目标保留既有 `inspect`/写入行为。链接文件 `write`/`patch` 的拒绝同时保护既有原子替换合同；原位写入因中断时可留下部分内容而未获支持。
- 当前 Windows 测试覆盖相对文件 symlink、循环拒绝、嵌套 junction、缺失 suffix 拒绝、junction 重定向检测、确认 receipt 绑定与进程 epoch 失效、已有路径 grant 不能自动放行链接、确认后的实际文件读取和重定向拒绝、目录直接项/子树范围绑定、已确认目录 capability 上的 list/search，以及目录 capability 不跟随内部 junction；复制/移动任一端通过链接时整体拒绝且无副作用。解析与 Files capability open worker 每引擎最多 4 个并发，等槽位最多 1 秒，deadline 为 3 秒；取消/超时发出协作标记并在路径组件边界释放资源，单个同步 Windows 文件系统调用不能被中途打断。Files read/inspect/list/outline/internal search/text summary 的直接 handle open 已纳入同一容量和 deadline；list/search 使用预先打开的目录或文件 capability，异步读、字节/行读取及 inspect hash 响应取消。未知/损坏 tag、解析期间和执行期间并发替换、拒绝/应用关闭/策略变更、多次超时后的资源压力、搜索/摘要主体的总时限、共享跳转和不同 Windows 文件系统/权限环境仍待验收。
- 新文件经已存在链接目录的原子创建仍未实现。Windows 平台原型验证了“目标目录只读解析句柄 → 普通目录 capability → `atomic_create_if_missing`”会在最终相对句柄提交时因共享模式冲突失败；原型已撤回，没有公开或未接入的安全 API 留在代码中。下一步必须先完成上述独立目录写 capability，再接入一次性 Files `write(expected_hash="missing")` 授权、精确目标策略和确认界面；现有 `write`/`patch` 原子替换语义保持不变。
- 当前工作切片不等于本 ADR 全部完成；阶段状态留在项目路线图，本 ADR 在所有验证条件通过之前保持未完成。
- 解析器目前与现有路径 capability 同处 `crates/platform/src/filesystem.rs`，以共享句柄和身份原语。该文件已超过开发规范的 800 行复核阈值；在扩大支持范围前，应把本地目标解析拆入私有子模块并保持 facade 不变。
- 授权、receipt、执行 proof 与 Files 读取 capability open 运行在同一每引擎有界 blocking worker 池；实际读操作消费打开后的句柄，list/search 也从预打开的 capability 起步。读、字节/行读取、inspect hash 和摘要输入扫描的异步 I/O 响应工具取消；搜索按条目预算、协作取消和外层工具执行时限运行，摘要按外层工具时限包住输入读取与摘要请求，并对 provider 请求应用单独时限。任一同步 Windows 文件系统调用不具备硬中断保证；open deadline 到期后调用会拒绝，worker 保留一个并发槽位直到当前 API 返回，再在下一个取消检查点释放句柄。

## 验证

完成条件包括真实 Windows NTFS 上 symlink/junction、嵌套和相对/绝对目标、缺失父目录、目标替换与链接替换竞态、授权/拒绝优先级、多资源无部分副作用、取消/超时/重启、UI receipt、严格 Rust 与 UI 门禁。网络测试使用隔离或明确标记的 Windows 环境；未经批准不访问真实共享。实现未完成前不得把 ADR 状态、路线图或回归矩阵标为支持已验收。
