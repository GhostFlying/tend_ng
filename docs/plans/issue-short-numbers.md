# Issue 永久编号

每个 Issue 获得全局唯一、永久递增的正整数编号。父项、子项和终态记录共用编号序列。用户在看板、概览、讨论、主 Chat 和通知看到 `#N`，可用 `/issues/N` 和 `/issues/N/overview` 分享入口。已有 UUID 链接继续有效。UUID 继续用于协议身份、写入关联和操作回执。

旧 Tend 在 [83bc15ae](https://github.com/GhostFlying/tend/tree/83bc15ae) 用独立的 `issue_short_numbers` 表、SQLite AUTOINCREMENT、插入触发器和不可变触发器分配编号，并在人工查询边界解析引用。NG 沿用这个存储方式，显示使用 `#N`。当前主干的版本 23 已用于待安排 Issue 迁移，本次在版本 24 一次性按 `issues.rowid` 为全部历史记录补号。后续按同一事务中的实际插入顺序分配。重放读取原 Issue 的编号，已分配编号独立于行号保存。

Server 在用户 Issue 路由入口统一解析编号。Router 读取和转达时解析编号并沿用 Chat 归属约束。CLI 在拼接路径前将 `#N` 转成 `N`。读模型在原查询中带出编号。Web 使用编号链接并保留旧数据的 UUID 回退及消息锚点。

验证包括版本 22 和 23 升级、两份历史库补号一致性、重启、不可变约束、并发插入、派发和委派重放、数字和 UUID API/CLI 路径、Router 读取和转达。使用真实 Server、固定 App Server 输出和 Chromium 验证看板、主 Chat、通知、概览和讨论。核对桌面与手机视口的导航、消息锚点和页面宽度，保存实际渲染的原始截图。运行 Rust 和 Web 工具链检查及受影响的端到端回归。

迁移用例 `tests/e2e/issue_short_numbers.py` 使用旧版本 Server 创建真实库。用 `62e11d9` 构建版本 22，用 `538dfb2` 构建版本 23，将二者的 Server 路径分别传给 `--v22-server` 和 `--v23-server`。可用 `--v23-cli` 同时验证旧 CLI 读取新 Server。浏览器回归使用 `tests/e2e/pull_board.py --web` 和 `tests/e2e/issue_chat.py --web`，前者可用 `TEND_ISSUE_NUMBER_SCREENSHOTS` 指定截图目录。
