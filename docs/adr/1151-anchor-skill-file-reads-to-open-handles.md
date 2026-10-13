# ADR 1151：以打开句柄固定 Skill 文件读取

## 状态

已采纳并实施；2026-10-13 workspace 自动门禁通过。

## 背景

ADR 1146 保留了一个局部竞态：Skills 先检查 `SKILL.md` / `requirements.txt` 的 canonical path，再按 pathname 打开；requirements 指纹计算与 pip 安装也可能读取不同版本。Skill 入口脚本在 venv 准备后由子解释器按 pathname 打开，同样会受并发目录改写影响。

同一用户可以改写自己的 Skills 目录，因此这里不构成 Windows 权限边界。仍需保证一次扫描或执行使用的内容与已校验文件一致，避免正常并发编辑把路径检查和读取拆成两个对象。

## 决定

- 对 manifest、requirements 和入口脚本先打开文件，再解析路径并比较打开句柄与当前路径目标的文件身份；路径必须仍位于对应 Skill 根目录内。身份比较在 Unix 使用设备号和 inode，在 Windows 使用卷序列号和文件索引；无法确认身份时拒绝读取。Windows 打开时只共享读取，避免现存写入句柄冲突，并阻止新写入或删除句柄直到 snapshot 完成。
- `SKILL.md` 有界读取与 Skills 文件指纹均从已验证的句柄取得，不在校验后按路径重开文件。
- requirements 从已验证句柄读取一次，最多 1 MiB。该字节快照同时用于 SHA-256 指纹和临时 requirements 文件；pip 只读取该临时文件，fingerprint 仍仅在 pip 成功后写入。临时文件创建在 Skill 根目录，保留 requirements 相对 include 的目录上下文，并在安装结束、失败或取消时清理。
- 入口脚本在 venv 准备前打开并校验，从该句柄流式复制最多 16 MiB 到唯一临时快照；Python 执行快照。启动器保留原脚本的 `sys.argv`、`__file__`、`sys.path[0]` 与 stdin 行为。这样目录更新不能在校验后替换解释器打开的脚本，超限脚本也不会触发 venv setup。
- 每次操作拥有自己的文件句柄与临时快照；不共享可变快照。临时文件随操作生命周期清理。超过脚本上限、路径变化或无法确认文件身份时失败关闭。
- 此决定只固定本次读取内容；Skills 仍是当前 Windows 用户信任的代码，Job Object 只管理进程树，不提供文件系统沙箱。

## 影响

配置、数据库、IPC 和持久化格式不变，无需重置。超过 16 MiB 的入口脚本现在拒绝执行。Skills manifest、requirements 和入口脚本在校验后不会再按原路径打开；requirements fingerprint 与 pip 使用相同字节快照。

## 验证

通过 `cargo fmt --all -- --check`、`cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings`。回归覆盖 requirements 外部 symlink 拒绝、pip 收到的临时快照字节与已验证内容一致，以及脚本快照/取消路径的既有边界测试。

## 回滚

不涉及持久数据或配置重置。回滚会重新引入 pathname 检查与后续打开之间的竞态；如果回滚，必须恢复句柄身份校验和有界脚本执行快照。
