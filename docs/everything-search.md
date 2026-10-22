# Everything 文件名搜索

Haven 可选使用 voidtools 的 ES 命令行工具查询 Everything 文件名索引。默认关闭；内容搜索始终由 Haven 原生后端执行。

## 配置

1. 安装并启动 [Everything](https://www.voidtools.com/)，等待首次索引完成。
2. 从 voidtools 官方发布渠道取得与所安装 Everything 版本兼容的 `es.exe`。
3. 编辑 `%APPDATA%\haven\config.toml`，填写 ES 可执行文件的绝对路径：

   ```toml
   [everything]
   enabled = true
   command = "C:/Tools/Everything/es.exe"
   ```

   Windows 路径可使用正斜杠，或在 TOML 字符串中将反斜杠写成双反斜杠。配置必须是本机绝对路径；Haven 不会下载或捆绑 Everything/ES。
4. 重启 Haven 后，`files.search` 的 `filename` 模式会使用 ES。查询语法遵循 Everything，可能与原生文件名 glob 不同；输出中的 `search_backend` 标出实际后端。

## 索引范围与结果解释

Everything 的索引范围由 Everything 自身管理。检查 **Tools → Options → Indexes** 中的 NTFS、文件夹和排除设置；只有列入并已扫描的范围才可由 Everything 查到。`es.exe -timeout 5000` 只等待 Everything 数据库加载，不表示首次索引已完成。

ES 返回零项时，Haven 输出 `index_coverage: "unknown"`、`search_complete: false`，并提示检查索引范围及首次索引状态。零结果不能证明 Haven 搜索根内确实没有匹配项。非空结果仍受请求的 `max_depth`、`ignore_hidden`、Git ignore 与文件读取授权筛选；被这些规则排除的结果不会显示。

## 常见故障

- **找不到 ES**：确认 `command` 指向 `es.exe` 而非 Everything 主程序，并使用绝对路径。
- **Everything 未运行或 IPC 不可用**：启动 Everything，确认托盘进程正常，再重试。
- **意外的零结果**：在 Everything 中检查索引范围与排除项，等待首次索引/重建完成，并确认该目录位于索引范围内。
- **结果与原生搜索不同**：Everything 使用自身查询语法；原生搜索还会按遍历规则应用深度、隐藏项和 Git ignore。可将 `[everything].enabled` 设为 `false` 以恢复原生文件名搜索。
- **配置未生效**：确认文件位于 `%APPDATA%\haven\config.toml`、TOML 语法有效，并重启 Haven。

## Windows 手工验收

发布或变更 ES 集成时，在安装 Everything 和官方 `es.exe` 的 Windows 环境逐项验证，并记录 Haven、Everything 与 ES 的版本：

1. 在已索引测试目录创建唯一名称文件，执行精确查询并确认 ES JSON 输出和 Haven 路径一致。
2. 搜索不存在的唯一名称，确认 Haven 返回空结果且 `search_complete` 为 false、`index_coverage` 为 `unknown`。
3. 关闭 Everything 后查询，确认返回 IPC/未运行错误，不伪装成空结果。
4. 将测试目录排除出 Everything 索引后查询，确认零结果仍标记覆盖未知；重新纳入并等待索引后确认命中。
5. 在搜索根内准备超深、隐藏、`.gitignore` 命中的文件及正常文件，确认 ES 后端遵守 `max_depth`、`ignore_hidden` 和 Git ignore。
6. 在不同官方 ES/Everything 版本组合上重复第 1、3 项，确认 JSON 字段与退出码兼容。

自动化测试不启动真实用户安装的 Everything；上述真实 Windows 验收必须在具备相应软件与测试索引的机器完成。
