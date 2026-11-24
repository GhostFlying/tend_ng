# Tend NG 总体设计

Tend 是单用户系统，支持在多个对话里并行推进工作。主 Chat 始终以同一个助手身份与用户对话。用户交代目标后可以接着聊下一件事，Tend 自动分发、跟进和收回结果。用户在原对话查看交付、追问和做决定，需要时再打开具体 Issue，从而减少切换上下文的开销。

本文定义产品范围、工作对象、执行边界及首期验收。各节链接对应的专题契约。基础选型与目录结构见工程文档，旧 Tend 的对照、决策依据与调研结果见设计决策记录。 [工程结构](engineering.md) · [设计决策与调研记录](design-questions.md) · [仓库工程约定](../AGENTS.md)

## Empower Agent

Tend NG 优先给 Agent 提供推进工作所需的工具、事实和权限。角色 Skill 和预置 prompt 写明 Agent 应遵循的做法，工作怎么拆、怎么验证、何时审阅或重试、怎样收尾，由责任 Agent 结合实际目标、仓库约定和已知证据决定。

首期从必要的最小约束开始。程序负责确认身份、责任与授权，保存事实并可靠投递，保障实际执行、工作区独占和副作用对账。用户、仓库或平台有明确要求时，在对应操作上校验所需事实。运行中发现确需约束的问题后，按实际影响增加范围最小的限制。

Tend NG 按可信单用户部署设计，信任用户、Agent 和已鉴权组件的使用意图。故障处理和验收只覆盖正常生产与运维中有具体发生路径的场景，例如断线、进程重启和响应丢失。新增保护以故障的实际触发条件和影响为依据。

## 1. 产品与交互

工作主链是 **Chat → Router Agent → 顶层 Issue 的 Issue Agent → 工作子 Issue 的 Work Agent**。Router 是用户的个人助理，代理主 Chat 中与用户的所有对话，负责排序、分发和组织回报。它根据原消息、明确的 Issue 引用和对话上下文定位工作，必要时新建顶层 Issue，再把实际委派和原消息关联起来。用户主动进入 Issue Chat 发言时，可以直接和对应 Owner 对话。

工作子 Issue 将交付引用和待决问题沿委派关系送达上级。各级 Agent 先处理自己能决定的事项，再将需要上一级决定的问题逐级上报。各顶层 Issue Agent 将各自的结果和需用户决定的事项提交给 Router。Router 在原 Chat 分别转述或组织汇总，按优先级及时限提出需要用户答复的事项，再沿原问题转回答复。 [结果怎样回来](chat-routing.md#结果怎样回来) · [逐级上报与用户决策](chat-routing.md#逐级上报与用户决策)

CLI 覆盖全部公开用户能力。Web 提供其中一部分，两者直接调用同一套 API，共用业务规则。Web 参考旧 Tend 的导航和布局，Chat 是日常工作台，看板是按需浏览的只读工作视图。看板的 Issue 卡片直接显示关联 PR 的可打开链接及已知状态。首期 Web 写入口集中在 Chat 和 Issue 的轻量讨论区，用文本和链接呈现进展、结果和待处理事项。

用户在 Chat 和 CLI 看到同一条 Attention 及处理状态。后台结果更新对应的消息，保留用户当前的输入焦点、草稿和阅读位置。通知独立展示 Issue 状态流转等只读动态，包含时间、工作和详情链接。首期通知采用无未读标记的呈现。

首期聊天渠道为 Web，使用普通 API 定期刷新。对话、消息、回复关系和 Attention 使用 Tend 的身份，未来渠道在收件和投递处接入。从 App Server 到页面的实时 SSE 链路留给后续全屏 Agent 干预。 [Chat 路由与回报](chat-routing.md) · [后续实时流](server-runner.md#后续实时流)

## 2. 工作对象与结算

### Agent 角色

| 名称 | 职责 |
| --- | --- |
| **Router Agent（Router）** | 由 Server 托管并直接绑定主 Chat 的具名 Agent。接收主 Chat 的用户输入、Issue 上报和归属未定的事实，负责判断、路由、跨 Issue 协调和回复用户。 [Router 托管与恢复](chat-routing.md#router-托管与恢复) |
| **Issue Agent** | 顶层 Issue 的协调 Owner，使用专属的 `issue-router`。职责限于保留用户目标和约束、委派子 Issue、跟进结果、汇总交付与决定顶层 Issue 何时结案。 |
| **Work Agent** | 工作子 Issue 的 Owner，负责技术计划、调查、实际执行、验证、产物和子 Issue 结案。按具体工作要求选择具名配置。 |
| **Review Agent** | 通过 `review` 分配的具名 Agent，担任审阅子 Issue 的 Owner 并交付独立审阅结果。 |

Issue Owner 表示当前 Issue 的负责人。Server 为顶层 Issue 和各子 Issue 分配各自的具名 Agent，Runner 持久领取后责任生效。Issue Agent 和 Work Agent 各自拥有派发、执行和 session 身份。持续推进期间沿用当前 Owner，必要时按原角色交接。

### 工作记录

| 对象 | 已确认的职责 |
| --- | --- |
| **Chat 消息与 Attention** | 保留原话、回复关系和用户决定。分发、结果及答复可追溯到原消息和实际责任人。 |
| **Issue** | Issue 及其持久关联记录是工作事实的权威来源。保存当前目标、Owner、决定、重要进展、等待、结果及证据引用。Chat 摘要、CLI 和看板读取同一工作事实。 |
| **Task** | Server 交给 Runner 的可独立执行委托，带目标、输入、授权和预期终点。同一委托在等待、续跑和重试中沿用原 Task。Work Agent 可显式结算，Runner 也可根据正常结束并已停妥的 Run 补记结算。 [Task 结算](task-lifecycle.md#run-正常结束时的-task-结算) |
| **Run** | Runner 对 Task 的一段实际执行。保存启动、原生回合、结果和停妥证据。一个 Run 可以包含多个 Codex 回合。 |
| **Codex session** | 执行上下文及历史，可跨 Run 复用以提高缓存命中率。新 session 从本轮输入和持久工作摘要接续，具体记录按需查询。 [Session 接续](agent-tools.md#session-接续) |

通过 Tend 委派工作统一走子 Issue。顶层 Issue 的实际工作由工作子 Issue 承接，简单工作也走这条路径。父子各自保存目标、Owner 和结果。

Issue Agent 取得本地委派回执并处理完手头工作后可结束本轮，停妥后归还 slot。子 Issue 的业务状态改变、Agent 留言及新交付必达每一级上级，直到顶层 Issue Agent。顶层结案后同样收到这些输入并唤醒 Issue Agent，由它决定是否重开。晚到的原记录及结果保存在来源子 Issue。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒)

Work Agent 可在任意阶段提交产物。各 Issue 的 Owner 显式决定自己负责的 Issue 何时结案。父 Issue 可在子 Issue 仍在推进时由 Issue Agent 结案，子 Issue 继续按各自的实际状态记录。Work Agent 决定实现范围内的审阅安排，复杂任务的预置 prompt 要求寻求独立审阅。Codex 原生 subagent 的审阅记录保存在当前工作子 Issue，委派给 Review Agent 则创建 `review` 子 Issue。 [工作委派与结案](task-lifecycle.md#工作委派与-issue-结案) · [独立审阅](task-lifecycle.md#独立审阅)

由 Tend 委派的审阅将所审候选、结论和证据回传发起方，并按原委派关系逐级上报。发起方按实际发现决定后续修复或复审。审阅执行按通用 Task 规则结算，审阅结论以实际交付的记录为准。 [独立审阅](task-lifecycle.md#独立审阅)

取消整个 Issue 时，Issue Agent 沿委派关系逐级转达，预期停止取消范围内的工作。停止控制发出后，Issue Agent 可以记录父 Issue 已取消。子执行保持真实状态，由 Runner 根据停妥证据更新。父 Issue 可以在旧子执行停止待确认时重开，旧停止控制继续作用于原执行。 [取消与逐级停止](task-lifecycle.md#取消与逐级停止) · [重开后的接续](task-lifecycle.md#重开后的接续)

## 3. 任务分配、执行与工作区

Server 和 Runner 各自持久化、启动和恢复。首次为顶层 Issue 选 Runner 时，Server 核对工作已明确必需的 repo 和能力，协调 Owner 使用 `issue-router`。工作子 Issue 按 repo、category 和 capability 等实际要求分配 Work Agent，必要时指名。具名 Agent 有稳定名称及实际执行配置，包括 harness、模型和推理强度。分配考虑已有的 workspace 归属，以及 Runner 上报的 repo、能力和可用 slot。首期支持显式请求 `image_gen`。 [具名 Agent 与任务分配](agent-selection.md)

Host 和 Runner 各有稳定身份。Server 可以按 Host 查看同一台机器上的多个 Runner，任务和执行事实归实际 Runner。 [Host 与 Runner 身份](server-runner.md#host-与-runner-身份)

Server 收到派发后以当前事实立即尝试匹配，并持久记录分配。Runner 定期上报可用性并拉取分给自己的任务。已确认未投递的预分配到期后可以撤回，首次投递与撤回互斥。接收结果未知时沿原分配查证。Runner 持久接收后，可以在与 Server 断联期间在本地推进到约定终点并保存结果。 [分配、投递与预分配到期](server-runner.md#分配与投递)

顶层 Issue 首次持久领取确定整棵 Issue 树的 Runner 和 Host。常规委派、续跑和重开沿该归属调度。容量不足时排队，Runner 不可用时等它恢复。各顶层 Issue 可以独立选择 Runner。同机并行接替时，后继可在原 Run 停妥并完成交接后，于原 workspace 启动同一 Task 的下一次 Run。 [Issue 树的 Runner 归属](server-runner.md#issue-树的-runner-归属) · [同机 Run 交接](server-runner.md#同机接替时的-run-交接)

slot 的运行统计反映 Runner 已确认正在执行的 Agent。等待、派发和回传各有持久状态。Runner 以本机的实际准入和停妥证据确认可用容量。 [执行 slot](server-runner.md#执行-slot)

Repo 在 Runner 注册，Git 访问及实际 workspace 由本机管理。需要代码时，顶层 Issue 默认共用一个 workspace，可在根目录下挂载多个 repo 的 worktree。Work Agent 从 workspace 根启动，沿外层 `AGENTS.md` 找到各仓库并遵循各仓约定。Issue Agent 协调子项顺序和交付衔接，按需指定独立 worktree 及其起点。新 worktree 在 fetch 后沿明确指定的起点或仓库登记的默认主干开工。Runner 管理托管目录的写入权和回收，写入权的交接以实际退出或隔离为依据。首期通过 Codex App Server 以非沙箱模式运行。 [Repo 注册](server-runner.md#repo-注册) · [多仓目录布局](server-runner.md#多仓目录布局) · [新 worktree 的起点](server-runner.md#新-worktree-的起点)

顶层 Issue 结案后保留 workspace。一期按 CLI 显式请求回收，由 Runner 核对实际目录占用及执行停妥情况。 [Workspace 保留与回收](server-runner.md#workspace-保留与回收)

Agent 在 Skill 指引下使用本机注入的 Tend 工具。Router 直接在 Server 本地提交操作。Runner 上的 Agent 由 Runner 持久受理并立即返回操作 ID，Server 异步应用并按原 ID 回传结算。Issue 关联与 Owner 输入都已持久入队时，Chat 显示「已派发」。 [Agent 工具与本地受理](agent-tools.md)

## 4. 消息投递、外部事件与等待

Issue 详情显示近期讨论、当前 Owner 和轻量输入框。父 Issue Chat 将自身和下属各级子 Issue 的留言按时间合并展示，保留原作者和所属 Issue。派发和新输入保存在各自的目标 Issue，回复沿原留言归属送达。 [Issue Chat 合并展示](chat-routing.md#issue-chat-合并展示)

Agent 通过显式留言工具把正文写入 Issue Chat。执行过程在同一时间线默认收起，用户点击后按原 Run 从实际执行实例加载。 [Issue Chat 的留言与执行过程](chat-routing.md#issue-chat-的留言与执行过程)

| 投递方式 | 行为 |
| --- | --- |
| **Queue** | 默认方式。消息持久入队。Owner 空闲或在 `waitfor` 期间，按输入的投递时机请求唤醒。已有活跃回合时在下一轮处理。普通委派和 Issue 补充使用这条路径。 |
| **Steer** | 投递到指定的当前原生回合。已证实回合在投递前结束时，界面显示未送达，用户可把原消息改为排队。接收与消费状态沿原消息和原回合查证。 |

主 Chat 的普通 Router 回合按对话串行。用户消息即时收件，选择下一轮时优先于普通后台回报。 [Router 的下一轮输入](chat-routing.md#router-的下一轮输入)

### PR 与 Issue 关联

一期保留两个关联入口，用户通过 CLI 提交，Agent 通过注入工具提交。

- **直接登记 PR URL。** 把明确的 PR 链接关联到目标 Issue。
- **按工作分支补查。** 引用目标 Issue 树中已记录的 Task 或托管 worktree，Server 按核实过的仓库和已记录的工作分支定向查询，确认唯一匹配后关联。共用分支时，分支用于找到候选 PR，具体子 Issue 归属由明确登记的目标和工作来源确定。

两条入口按目标 Issue 和 PR 身份去重。关联一旦持久保存就可在看板打开，状态随 Server 后续采集更新。

一期在工作 Task 到达终态且 Runner 已确认实际停妥时自动补查。Server 收到这两项持久事实后，对尚缺对应 PR 关联的已记录工作分支发起一次上述分支补查。按原 Task 终态、repo 和工作分支登记一次自动补查。普通回合结束和 `waitfor` 暂停期间，原 Task 仍在推进或等待。补查异步执行，Task 的结算独立完成。后续由用户通过 CLI 或 Agent 通过注入工具按需发起。

### PR 和 CI 事实的采集与投递

一期围绕 Tend 的工作识别 PR。Issue 登记了关联 PR，Server 即将该 PR 纳入观察范围，为看板提供状态。`subscribe` 和 `waitfor` 也可以登记明确的 PR 或 CI 对象。Server 针对这些已关联或已登记的对象采集事实，保存来源、对象、版本与证据。

一期的 GitHub 采集由 Server 使用服务用户自己的只读权限。已关联或明确登记的对象进入观测范围，私有仓库须能从 Server 读取。Runner 分别持有本机 Git 访问权限。访问失败时保留 PR 链接、最后一次读到的事实和当前观测状态。依赖该对象的等待在确认权限缺失后，将原因送回原等待 Agent，供它决定后续。

看板读取已保存的关联和状态。符合订阅、等待或必送规则的事实按已有 Issue 关联和收件登记进入待投递队列。默认路径用于尚未指定收件人的投递。CI 条件可以限定候选提交、检查名和实际运行或 attempt。

| 路由依据 | 投递路径 |
| --- | --- |
| 命中有效的 `waitfor` | 经当前负责原 Task 接续的 Runner 直接交给登记等待的 Agent，每一层都适用。 |
| 命中有明确收件人的订阅 | 沿原订阅投递给登记的 Agent。Router 由 Server 本地投递，其他角色经各自的 Runner 投递。 |
| 默认投递，已有 Issue 归属 | 经该 Issue 树所属 Runner 交给顶层 Issue Agent，保留关联的具体 Issue。 |
| 默认投递，Issue 归属未定 | 进入 Server 本地的 Router 队列，由它判断如何关联和跟进。 |

同一事实符合多笔已登记的等待、订阅或必送约定时，分别沿各自记录送达。每笔 `waitfor` 的续跑直接由原等待收件人接续。需要经 Runner 投递的输入在 Server 持久排队，Runner 离线恢复后沿原目标送达。 [Server 与 Runner 契约](server-runner.md#派发与本地接收)

- **Optional** 由 Agent 主动订阅。普通观测可以更新可查询的工作状态，命中订阅的事实进入待投递队列。
- **Required** 由明确的工作绑定或约定定义。符合条件的事实最终送达该约定的收件人。
- **合并唤醒** 已有归属的外部输入按 Issue 合并，归属未定的按 Router 的收件队列合并。从第一条待投递事实起算固定五分钟，每条事实保留自己的身份和证据。用户消息及 Attention 答复及时进入对应输入队列。

窗口到期后按实际收件目标递送，同一目标每批至多唤醒一次。收件人已在执行时，事实可进入已有对话的待收输入。重复观测沿原事实和收件关系去重。

一期的 PR / CI `subscribe` 和 `waitfor` 共用明确的事件匹配条件。`subscribe` 把命中的事实递给订阅者。条件尚未满足时，`waitfor` 同时登记条件和当前 Task 的暂停意图，Runner 确认执行停妥后归还 slot。原 Task 继续独占其已分配的工作目录。条件命中后，原 Task 随外部事件批次重新排队，取得 slot 后在原目录续跑。 [任务等待与续跑](task-lifecycle.md)

等待期间，用户的普通补充、改变方向的消息和 Attention 答复及时唤醒对应 Owner。原 Task 取得 slot 后处理新输入，Owner 根据原等待状态和新事实决定下一步。 [等待期间的用户输入](task-lifecycle.md#等待期间的用户输入)

## 5. 同步、持久化和恢复

Server 和 Runner 通过定期请求交换任务、控制意图、事实和回执。全量对账从持久记录补齐原任务、进展、结果和回执，可以独立支撑同步和恢复。首期普通增量 API 按持久确认位置交换新增记录，Chat 以完整消息为单位。任务进行中也能同步已落盘的进展和完整消息。最后联系、实际进展、同步缺口和停止证据各有对应的可见状态。 [Server 与 Runner 契约](server-runner.md)

断连恢复的全量对账覆盖已有的未结算 Task、待同步记录和明确缺项。历史 Task 按已发现的缺项或显式请求核对。Task 间独立对账，领取和控制在恢复通信后照常进行。 [一期同步 API](server-runner.md#一期同步-api)

Chat 和 Issue 讨论的完整消息以及 Issue 当前状态由 Server 数据库直接持久保存。交付文件的原件独立同步到 Server 的文件存储。Server 可以先显示 Task 已完成和文件同步中，原件完整持久保存后再提供下载。Codex 原生执行记录留在实际执行实例，Router 由 Server 本机保管，工作执行由原 Runner 保管。Web 打开详情时通过共用 API 按需读取。 [数据库存储与访问边界](storage.md) · [按需读取 rollout](server-runner.md#按需读取-rollout)

交付文件在原 Runner 下一次可通信的后台同步中开始独立上传。Runner 在 workspace 之外保留已同步的交付副本，Server 发现原件缺失后，经后台同步通知原 Runner 按原文件身份补回。交付副本的自动清理留待后续。 [交付副本与全量补回](server-runner.md#交付副本与全量补回)

受影响的状态、可按原 ID 查询的操作结果和必需的待投递记录在所属实例的同一个短事务提交。Chat 按页取消息并批量取关联，看板分页取 Issue 当前状态和所需的 PR 关联。 [数据库存储与访问边界](storage.md)

两种同步路径共用原始记录身份。断线、回执丢失或 session 替换后，Issue、原消息、操作和 workspace 保留各自的身份。外部操作结果未知时，先沿原请求查证，再依据结果决定下一步。

部署可选外置 tsnet、LAN 直连或宿主机 `tailscaled`。各方式对等承载业务请求和同步，信道按可信处理，Tend 请求使用预共享 token。Server 和 Runner 一期各自在本机顺序替换，接受短暂断线。新旧 Runner 并行接替时各自完成鉴权和同步，可共用独立运行的外置 tsnet 通道。后继接新顶层 Issue 和已交接的续跑工作。前驱在旧 Run 停妥、续跑交接及回传收尾后自然退役。 [通信通道与独立升级](server-runner.md#通信通道与独立升级)

Server 兼容当前与上一代 Runner 通信协议。Server 升代前核对未退役 Runner 的持久登记，发现低于目标窗口的 Runner 时，在停止当前 Server 前拒绝升级。 [Server 升级兼容窗口](server-runner.md#server-升级兼容窗口)

## 6. 工程结构与首期验收

一期 Server、Runner 和 CLI 采用 Rust，tsnet 由独立 Go 进程提供。Server 和每个 Runner 各自使用本地 SQLite WAL。开发和测试在 devcontainer 中进行，前端工具链以旧 Tend 为基线。 [数据库选型](storage.md#选型和验收) · [工程结构与基础选型](engineering.md)

主要场景使用固定输入与 App Server 输出 fixture，启动真实 Server、Runner 和临时 workspace。测试断言用户可读的结果、文件变化、原始投递身份和执行事实。

第一次试用验收 Router 从主 Chat 接单到把答复和产物交回原话题。一期还必须验收 Attention 逐级上报给 Router、由 Router 在原 Chat 提问和转回答复。 [首个产品切片](plans/first-product-slice.md)

| 场景 | 验收结果 |
| --- | --- |
| **一个 Chat 并行推进两项工作** | 用户交代 A 和 B 后继续写 C。Router 分发给各自的 Issue Agent，分别委派 Work Agent 推进。两项分别上报给 Router。用户在原话题从同一个助手收到一次 A 的结果，并收到 B 的问题，可以直接追问或作答。CLI 读取同一状态。Issue 流转可在通知查看，C 的草稿、焦点和阅读位置保留。 |
| **Issue 讨论和回合控制** | Issue 中能读到 Router 的派发和 Agent 通过工具明确提交的留言。执行过程默认收起，点开才读取对应 Run。Queue 在下一轮消费，Steer 进入指定的当前回合。模拟回合更换及回执丢失，验证原输入的去向能够查证。 [Issue Chat 验收](plans/first-product-slice.md#一期-issue-chat-验收) |
| **外部事实批次与等待** | 用 URL 或工作分支关联 PR，核对看板上的唯一关联、链接和 Server 已采集的状态。复用同一输入验证漏登记时只在工作 Task 终态且停妥后安排一次自动补查，重放沿原操作。再为不同层的 Agent 登记 `waitfor`，核对匹配事实直接到各原等待者。默认投递经 Runner 到 Issue Agent 或 Router。加入重复事件和旧候选结果，核对同一目标每批只唤醒一次、原事实均可查。等待期间 slot 可供其他工作使用，原 Task 保留目录占用并在原目录续跑。 |
| **离线执行与同步恢复** | Runner 在断联时推进已领任务到约定终点，工具提交能取得本地回执。重连后仅用全量对账补齐完整留言、交付文件、结果和回执。重复同步产生一份可见结果。Runner 再次离线时，Server 仍可提供已同步的留言和文件，rollout 详情显示原 Runner 的读取状态。替换 session 后从原 Issue、输入和回执继续，资源释放依据实际停妥证据。 |

先用一条 fixture 跑通 Router 从 Chat 接单、Issue Agent 委派、Work Agent 交付、Issue Agent 上报和 Router 在原话题回报，再补投递、等待和离线恢复。局部修改运行对应模块测试，共享接口变化验证受影响的边界。首期以少量端到端 fixture 和本地定向验证支撑迭代，CI 卡口在系统稳定后再定。
