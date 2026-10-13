# ADR 1155：将文件读取授权绑定到打开句柄

## 状态

已采纳并实施；受影响 crate 门禁通过。workspace 门禁被本切片外的当前工作树失败阻断。

## 背景

`AuthorizationEngine` 会解析文件路径、检查允许根和 reparse point；执行入口也会在调用工具前重查。但工具如果随后再次按 pathname 打开文件，两个检查都无法证明读取的仍是同一个文件对象。并发路径替换可让授权对象与读取对象分离。

## 决定

- 普通文件读取先打开并持有只读句柄，再解析当前路径、检查 reparse point 和 `allowed_paths`，并与该路径重新打开的句柄比较文件身份。相对、UNC/device、reparse、身份不匹配或无法解析的路径一律拒绝。
- 校验通过后，读取、摘要、outline、inspect/hash 和内容搜索只使用已验证的句柄。不得在校验后按原 pathname 重开文件。
- 路径允许根从当前 `AuthorizationEngine` 配置捕获；父级与操作级设置都必须满足。文件搜索对每个候选文件重复句柄校验。
- 平台层在 Unix 使用 `O_NOFOLLOW` 并以 device/inode 比较身份；Windows 打开时拒绝新写入与删除共享，并使用 `FILE_FLAG_OPEN_REPARSE_POINT`，再以卷序列号和文件索引比较身份。平台无法提供稳定身份时 fail-closed。
- 受管媒体读取也通过 asset registry 打开、验证并消费同一个句柄。Registry 注册时保存文件身份；普通路径转入 managed asset 时绑定到通过路径策略校验的源句柄，读取时身份变化即拒绝。

## 影响

数据库、配置、IPC 和持久化格式不变，无需重置。此决定保护文件内容读取的对象身份；它不提供 Windows 权限边界，也不改变 Skills 是当前用户信任代码的事实。

## 验证

回归测试覆盖句柄打开后 pathname 替换、允许根外文件、reparse point、managed asset 注册后替换，以及验证后继续从原句柄读取；Windows 用例验证共享模式阻止替换。`cargo fmt --all -- --check`、受影响 crate（`haven-platform`、`haven-tools`）严格 Clippy 和 `cargo test --locked -p haven-platform` 均通过。`cargo test --locked -p haven-tools` 中 851 项通过、2 项忽略、2 项失败（Skills 管理投影的 Windows 路径前缀断言、Skills runner 输出上限断言）；workspace 测试在 `haven-skills` 45 项通过、1 项失败（Windows 路径前缀断言）。workspace Clippy 失败于本切片外的 `haven-app-binary` 启动代码两项 lint。UI 检查和 production build 通过；UI 测试 1281 项通过、2 项失败（SettingsView 超时、命令目录数量断言）。这些工作区失败不属于文件安全切片；本次变更按受影响 crate 门禁提交。

## 回滚

不涉及持久数据或配置重置。回滚会重新引入 pathname 授权与后续打开之间的竞态；若回滚，必须同步恢复句柄身份验证和同句柄读取。
