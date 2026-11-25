# Issue 详情与原讨论同页

## 目标和边界

- #25 在 #6 已合入的概览、看板入口和 Issue Chat 基础上推进。检查工作区和 refs，未见另一个 #25 实现候选。
- 看板进入 `/issues/<短号>`。保留 `/issues/<id>/overview`、UUID、短号、原 Chat 和消息锚点入口，全部 Issue 路径显示同一组件，不把读者跳到第二套聊天页。
- 主栏先显示标题、当前阶段、目标摘要、已回传的报告和材料、待决定事项，再显示原时间线和直接回复入口。次要属性在桌面靠右，手机折叠在一条阅读流中。不改消息投递、评审/结案规则，不做 Token 统计，不在生产执行写入或部署。

## 参考和取舍

- 查阅了 [Multica Issues 文档](https://multica.ai/docs/issues#deliverables) 内的详情界面说明和 [固定版本 IssueDetail 实现](https://github.com/multica-ai/multica/blob/188971168d0e2a6fa7796f8825e2ed096ccc5fe3/packages/views/issues/components/issue-detail.tsx)。沿用标题和描述、右侧次要属性、同页 Activity / Comments、持续可用的底部回复入口。Multica 的 Deliverables 在源码中位于侧栏，Tend 将用户要审阅的确切交付推进主流，不按同名归并版本。
- 普通打开显示页面顶部及最新一页原讨论。消息锚点复用现有 `before` 游标逐页寻址，保留原留言归属、原 Chat 链接和引用回复规则；支持同页报告消息锚点。

## 数据关系

- 生产 GET / SQLite 只读检查显示 #6 的概览与聊天入口独立，#8 的直接 Issue DTO 没有两份原件。树概览的普通附件列表不区分草稿。主 Chat 的正式物料消息 `9b945705-f2f1-4b32-9c90-49f045bd19c3` 通过 `chat_artifacts` 关联两个确切原件，来源分别是 #19、#21 的 Task。后续终验消息另附一份证据，不能按最近消息选择报告。
- 用所点选 Issue 的原请求和所属 Chat 限定回复消息。用消息的持久附件关联与该 Issue 已完成上报的 `router_inputs → issue_reports → report_artifacts` 交集证明关系，再核对交付归属、Task、发布状态和树边界。以消息为单位保留分组和来源；日报、任务日志、过程附件与未被报告关联的同名草稿不充作报告原件。旧记录若没有可证明的消息关联，显示空态，不根据正文、日期或文件名回填、推断“最终”或伪造待审状态。
- Markdown 用禁用原始 HTML 和图片/远程资源的渲染器，限制链接 scheme，表格和代码在自己的区域滚动。只在用户展开时读取有限大小的 Markdown 原件。HTML 只走现有鉴权下载；不嵌入页面，不执行原件。查看待决定事项沿真实 Chat 问题链接，仍从原 Chat 提交决定。

## 验证

- Rust 真实 SQLite schema fixture 覆盖结案历史、同名未引用草稿、错 Chat/同话题其他 Issue、未发布或不可用原件、身份来源、零报告；HTTP 检查无令牌访问。
- Web 用 Chromium 桌面和 390×844 从看板进入并验证 legacy alias、同页 Markdown/窄屏滚动、HTML 下载、真实来源和原 Chat、游标深链和原留言回复。回归测试包含危险链接、原始 HTML 和图片不执行/不发请求。
- #6/#8 仅读生产旧入口和持久关系，新版在隔离数据库中验证，生产不发送回复。交付两张真实浏览器渲染图并区分生产核实与本地验收。运行 Web lint/typecheck/build、Web 回归与 Rust 定向测试。

## 复核后的锚点修复

- 接续 `6ab8f232f5be818cb9703115a040f9ffd3f10bfb`，只调整 Web 历史分页和概要恢复。手动和自动读取共用分页请求与游标，进行中的手动分页先完成，锚点再从已更新的游标继续。切换锚点或离开页面时取消旧请求，保留统一的消息去重。
- 首次概要读取成功之前，普通手动翻页仍可用，但不自动翻页或判定锚点不存在。概要重试成功后先检查同页报告及已加载的讨论，再按需读取历史。找到锚点时清除定位错误，独立的手动翻页错误保持原样。
- 报告 SQL 不变。`6ab8f23` 的 CTE 由报告文件和附件索引缩小候选，避免了复核中一种查询计划的无关附件全扫。默认未 `ANALYZE` 的 bundled SQLite 3.50.2 fixture 中，外层仍可能从 `chat_messages` 索引取候选，读取代价仍受同 Chat 消息规模影响。该验证不代表生产延迟或与其他话题历史规模无关。
- 在真实隔离 Server 的 Chromium 1440×1000 和 390×844 fixture 中控制概要和历史响应顺序，核对交错续读、游标不重复、滚动与后续手动翻页。注入概要 503 再恢复真实 200，分别验证报告和讨论锚点，并在概要仍失败且讨论已读尽时确认没有缺失提示。保留普通打开、普通翻页、深链和引用回复验证，运行 `pnpm lint`、`pnpm typecheck`、`pnpm test`、`pnpm build` 及 `issue_detail`、`pull_board`、`issue_chat`、`delivery` e2e。
