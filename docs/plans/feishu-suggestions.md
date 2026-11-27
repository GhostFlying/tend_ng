# 飞书建议一期（#130）

## 目标与对照

桌面和移动端保留独立的「飞书建议」入口，CLI 与 Web 共用用户 API。以 LDD 的 `modules/followups/AGENTS.md`、`contracts/openapi.yaml`、`ingestion.go` 和 `integrations/larkmessages/client.go` 的实际契约为参照（devbox 源码版本 `81c91df8260d00d534413eaf25225435ba2d1415`）。候选必须是非本人发送、明确 @ 当前用户的消息，不能以搜索命中或 @全员 代替。按身份和来源消息 ID 去重。优先按会话与 thread 分组，其次用可追溯的回复根，缺少可靠 ID 时按 LDD 的同一会话 30 分钟间隔、两小时上限聚合。不把同群无关话题作为新进展。未显式带入 Tend 的采集材料保留七天，显式引用或转交的快照由对应 Tend 记录保存。

## Server 采集与恢复

- Server 独立配置正式 `lark-cli` 和固定用户 open ID，始终用 `--as user` 复用该 profile 的现有 OAuth 会话，由 CLI 负责刷新。只调用身份检查与消息读取；不接管凭据，不发送飞书消息。
- 用 `lark-cli api POST /open-apis/im/v1/messages/search` 以 `is_at_me` 分页拿到原始候选 ID，并用 `im +messages-mget` 精确回查。已核实 1.0.88 的 `+messages-search` 快捷命令只报告已成功补齐详情的数量，无法证明整个原始候选页都读到了。逐条核对结构化 mention、sender、消息时刻、会话及原文链接。用 `+threads-messages-list` 或 `+chat-messages-list` 为已捕获的话题增补同 thread 或有明确回复关系的消息；尽量保持有限、安静的后台采集。
- 本机单循环负责扫描，CLI 调用有期限，不在 SQLite 事务内执行。完整窗口中所有页成功才在一个短事务中写入来源、建议及 checkpoint，空窗口也算成功。失败或重启以最后成功 checkpoint 为准，短重叠补偿索引延迟。首次回看三天，已有 checkpoint 的停扫追赶按日窗口前进，不能因过去的 running 状态卡住。API 区分最近成功、扫描游标和当前阶段；不能完整翻页或读取来源时显式显示缺口。
- LDD 实况中健康服务八天没有扫描，旧轮停留在 running，worker 只领 queued，而旧轮又阻挡新轮。fixture 必须覆盖停扫八天、旧 running、进程重启、分页失败、成功空页和补扫查重。这里无需分布式队列，也不申请 Runner slot。

## 研判和用户动作

- 机械扫描不启 Agent。独立的 Server 本机研判循环仅处理证据版本有语义变化的建议，按版本合并并持久化研判状态，不调用主 Router、不派发任务。可选具名 Agent 的独立 category 为 `feishu-router`，默认程序 `codex-ark`、模型 `deepseek-v4.1-flash`、推理 `high`，可显式配置模型和独立运行目录。使用现有无原生副作用能力的受限 App Server，纯结构化研判，结合锚点后的回复识别已解决事项并移出待处理列表，核对证据 ID，未配置或失败时保留事实卡片，不冒充模型建议。
- 「带入讨论」用同一 API 选择已有讨论或新建。服务端把指定版本写成可追溯的飞书参考，不伪造用户发言、不自动开始 Agent 回合；后续可以补充、继续讨论，再沿 #117 显式转交。已有引用可继续打开或按新版本更新。
- 「转交主 Chat」把用户选中的证据固定在独立的转交上下文，直接复用 #117 的 Discussion Agent、持久快转请求和受限主 Router 接收。保留事实、链接、未知项与实际用户的补充（在对应讨论中转交时），事件快照与真实用户原话分开。点击、飞书原文、研判和生成摘要不构成实施授权，也不能代答 Attention。按建议版本去重和查原操作，显示主 Chat 生成消息的原链接，失败可查证及重试。直接转交内部上下文不出现在普通讨论列表。
- 忽略只针对当前材料版本，可撤销。有新材料时重新出现。带入讨论和直接转交分别保留关联与回执。
- 不接入通知、主 Chat 后台提醒、自动开 Issue、Owner 投递或 Runner 调度，不做部署。

## 验证与交付

Rust 定向测试和隔离的本机 CLI、App Server fixture 覆盖身份、分页与 checkpoint 恢复、去重、版本、回执和授权。Web 跑冻结依赖、类型、lint、格式、测试和构建，用真实本地 Server、合成材料和浏览器验证桌面与手机，并保存最终渲染图。遵守仓库直接推送 `origin/main` 的规则，做候选独立审阅、核对分支门禁和最终远端 SHA，合入即交付。
