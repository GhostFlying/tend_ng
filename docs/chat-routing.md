# Chat 路由与结果回到原对话

Router 的个人助理职责、主 Chat 与 Issue Chat 的对话边界、Attention 和只读通知的用途已确定。提醒时机继续用具体场景核对。

## 用户在 Chat 中看到什么

**已确认。** 用户在主 Chat 始终与同一个助手对话。Router Agent 接收全部用户消息、追问和 Attention 答复，并以同一助手身份在原话题送回答复、交付和需要用户处理的问题。用户可以留在原对话继续交流，需要时按引用查看工作详情。主动在 Issue Chat 发言时，可以直接和对应 Owner 对话。Web 和 CLI 使用同一套对话规则，消息与工作的关联可跨 session 恢复。

发送消息后，Server 持久收件，Chat 显示已收到。Router 在 Server 本地提交派发并取得操作 ID。Issue 关联与 Owner 的待投递输入已持久入队后，Chat 显示已派发。缺少合格 Runner 时，原派发记录显示待分配原因。Owner 开工、处理完成和结果送回 Chat 分别有实际证据。平常的排队和运行进展显示在关联工作上，用户需要时可以展开。 [Agent 工具与本地受理](agent-tools.md)

一期的回复和回报以完整消息进入各自的对话。Router 在 Server 写入主 Chat 的回复和回报。工作 Agent 的 Issue 留言在 Runner 落盘后即可进入下一批同步。 [Server 与 Runner 契约](server-runner.md)

## Router 托管与恢复

**已确认。** Server 使用本机 Codex App Server 托管绑定主 Chat 的具名 Router。主 Chat 输入、各 Issue 上报及归属未定的事实在 Server 本地入队，由 Router 按需消费。工作 Runner 全部离线时，Router 仍可查阅已同步的工作事实、答复用户和提交委派。原委派可以在合格 Runner 恢复时继续分配。

Server 从持久记录恢复 Chat 收件、历史读取和 Runner 同步，Router 在本机继续启动或恢复。Router 的 App Server 暂不可用时，保留原输入并显示已收到、待处理。Server 持久保存已提交的回复、派发和操作回执。恢复 Router 时沿原输入和可查的执行证据接续，session 可复用或重建。 [本地受理与业务结算](agent-tools.md#本地受理与业务结算)

## Router 的下一轮输入

旧 Tend 在同一主会话中优先接续已领取的投递，再取用户消息，随后取后台事件。各组按提交顺序处理。回归用例让后台跟进先入队、用户补充后到，验证先领取用户消息。 [旧队列排序](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/store/outbox.go#L56-L68) · [用户消息优先用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/background_test.go#L242-L254)

**首期已确认。** 同一主 Chat 串行启动普通 Router 回合，收件随时持久入队。选择下一轮输入时，已到达的用户消息优先于普通后台回报，用户消息之间按接收顺序处理。已有活跃回合时，新的 Queue 消息等待下一轮。 [Queue 与 Steer](design.md#4-消息投递外部事件与等待)

Router 空闲时按这个顺序选取已到达的输入。一轮可处理多条已到达的 Issue 回报，各自保留原身份和话题。Router 也可以在回复用户时读取相关交付或 Attention，按原约定和紧迫程度决定怎么说。普通工作进展仍可按需查阅。 [结果怎样回来](#结果怎样回来) · [Chat 内处理 Attention](#chat-内处理-attention)

复用一期 A、B 并行、C 继续聊天的 fixture。让 A 的交付在 Router 忙时先入队，随后收到用户的 C，核对 C 持久收件且先进入下一轮，A 的回报仍能沿原话题送达。再让 A 的处理回合先启动，核对 C 持久排队并在 A 回合结束后接续。 [一期验收场景](design.md#6-工程结构与首期验收)

**后续评估。** 吞吐增加后，依据用户消息的排队耗时和实际答复耗时，判断是否用 subagent 分担可异步完成的工作，例如查阅多项 Issue 的交付材料。分工需要让 Router 可以先给用户反馈、结束当前回合并处理下一条消息。子 Agent 的结果回到 Router，由主 Chat 的同一个助手组织答复。

## Router 输入的恢复边界

**已确认。** Chat 收件把原消息、Router 待处理输入和收件回执放在同一个短事务。取得持久的原生 thread 后，Server 在发出 `turn/start` 前记录这次尝试的标识、原输入和 thread，用尝试标识填写 `clientUserMessageId`。原生调用和历史读取在事务外执行。拿到 `turnId` 后沿原尝试保存，回执丢失时可从原 thread 的原生历史按尝试标识查证。 [实际 App Server 验证](plans/rust-offline-smoke.md#router-原生输入的历史关联验证)

| 恢复时持有的证据 | 接续方式 |
| --- | --- |
| Chat 已收件，输入尚未交给 App Server | 从原待处理输入启动 Router。 |
| `turn/start` 没有响应，启动结果待查 | 查原尝试的回合、本机停妥证据和已提交的操作。找到终态就沿原回合结算。确认旧执行已停妥且查不到可恢复的回合时，从同一条输入自动接续一次，保留两次尝试的身份。第二次仍无法恢复时，将该输入标记为处理失败，保留原文和回执，继续同一 Chat 的队列。 |
| Router 的派发已经提交，回合尚无终态 | 自动补发时标明这仍是原请求，携带原消息 ID、尝试编号及已提交的派发。Router 结合实际目标与回执判断哪些工作已经派发，可用 `tend_read_dispatch` 按原操作 ID 读取回执，再继续尚未处理的独立工作。 |
| 原回合已成功结束 | 读齐该回合的输出，按原生消息身份补齐应进入 Chat 的完整消息，再结算该次输入消费。已派发 Issue 的推进和结算沿各自的持久记录继续。 |

一个 Chat 输入可以产生多次合法派发。消息身份锚定用户话题，原生尝试身份关联一次 App Server 执行，业务操作 ID 则用于派发或回复写入的去重。Router 接续时从已提交的派发和回复恢复上下文。 [操作身份及首次回执丢失](agent-tools.md#操作身份及首次回执丢失)

进程仍运行或停妥证据暂不可用时，原尝试保持待查。同一 Chat 的实际执行保持串行，其他 Chat 的 Router 可以继续。原 thread 无法继续时，新 session 从已保存的对话和工作事实接续。用户可以在原 Chat 重新交代处理失败的请求。

旧 Tend 为原生输入保存关联，`turn/start` 结果未知时保留原投递。旧 #387 还暴露了终态查证受实时交付锁阻塞的路径。新版允许历史查证独立读取和补齐原尝试，收件、原进度读取及其他 Issue 的 Runner 同步继续走自己的持久记录。 [旧原生输入和启动结果](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/mainthread.go#L1468-L1505) · [旧历史回执取锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/turn_receipt.go#L225-L244)

## 从消息确定工作归属

| 用户的输入 | 建议行为 |
| --- | --- |
| 询问已有进展，或提出可以用当前事实回答的问题 | Router 可以直接回答，带上所依据的状态或结果。 |
| 发起需要调查、操作或产出的新工作 | Router 建立或复用顶层 Issue，Issue Agent 收到原目标和约束，并通过工作子 Issue 委派给 Work Agent。 |
| 在主 Chat 回复已关联 Issue 的消息 | Router 接收原话和关联，依据内容答复或沿原 Issue 转达。 |
| 主动在某个 Issue Chat 补充要求或追问 | 直接交给对应 Issue 的 Owner。回复子 Issue 留言时沿原留言归属送达。 |
| 同一条消息交代多个独立目标 | Router 可以分别委派。每项工作都有自己的归属，仍能从同一条原消息看到进度和结果。 |
| 确有歧义，选择不同归属会改变目标或授权 | Router 在原 Chat 提一个具体问题，收到答复后接着完成原分发。 |

同一目标的细化或后续要求优先沿原 Issue 推进。Issue Agent 保留原输入，把影响实现的补充送给已有工作子 Issue，技术步骤由 Work Agent 自主安排。复用依据可以是明确引用、实际回复关系或能核实的工作上下文。一次路由结果保存原消息、实际派发内容和目标 Issue，重复请求沿原关联恢复。

用户入口和 CLI 支持用 `#N` 或 `N` 查询和操作 Issue。Router 在当前 Chat 内可用编号或 UUID 读取、开始已记录的 Issue，或向其 Owner 转达消息。Server 在入口解析到原 UUID，保存原消息及 Issue 关联。

例如，用户说“调查 A 的崩溃，再整理 B 的发布说明”，Router 可以分别派给两个顶层 Issue。用户在主 Chat 引用 A 的结果补一句“顺便补上这个测试”，Router 带着原话和引用转给 A 的 Issue Agent。用户进入 A 的 Issue Chat 写同一句补充时，可以直接交给 A 的 Owner。

归属未定的待投递 PR / CI 事实进入 Router 的输入队列。Router 可以根据原事实和已有工作确定 Issue 关联，并决定后续行动。 [PR 和 CI 事实的采集与投递](design.md#pr-和-ci-事实的采集与投递)

## Issue Chat 合并展示

父 Issue Chat 参考 Multica 的交互，把父 Issue 和下属各级子 Issue 的留言按时间合并到一条时间线。每条留言显示原作者、所属 Issue，并保留原留言身份。合并时间线按页读取原留言。

在父 Issue 发布的新留言发给父 Issue 的 Owner。回复某条子 Issue 留言时，沿被回复留言的归属写入该子 Issue，并送给该 Issue 的 Owner。

用户留言按原收件人及时请求唤醒，等待中的 Owner 同样进入调度。已有活跃回合时，Queue 消息在下一轮处理。 [等待期间的用户输入](task-lifecycle.md#等待期间的用户输入)

## 结果怎样回来

Work Agent 将交付、证据和剩余事项保存在来源子 Issue。业务状态改变、Agent 留言及新交付沿委派关系从各级上级的持久队列送达，直到顶层 Issue Agent。各顶层 Issue Agent 对照原承诺，将结果和交付引用分别上报给 Router。Server 持久入队并保留来源 Issue 和原话题。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒)

Router 按交付约定和当前对话分别转述各 Issue 的结果，也可以重新组织成一条汇总。回报保留原话题及各项结果的引用，后续追问由 Router 沿回复关系接续。候选更新、CI 等待和普通进展保留在工作记录，用户主动问时可以得到当前状态。 同一原话题里，多份报告可以引用同一份原件。各份正文保留，下载入口按原文件身份只显示一次。

用户明确要求当前 Issue 交付计划、报告或附件后原地等待确认或审阅时，Router 在派发的持续目标中注明这是阶段交付，等待用户决定是目标的一部分。Router 在原 Chat 回报实际结果和附件，并发布相关 Attention。交付回执不是整个 Issue 的完成条件，顶层 Issue 保持开放直到用户作答后由 Owner 决定下一步。 [工作委派与 Issue 结案](task-lifecycle.md#工作委派与-issue-结案)

## Issue Chat 的留言与执行过程

**已确认。** Issue Chat 里的 Agent 留言由显式调用留言工具产生。用户直接在 Issue 中提问时，Owner 用同一个工具答复并关联原留言。留言按所属 Issue 与原作者持久保存并同步到 Server。派发、交付和 Issue 状态按各自的已提交事实展示。 [Agent 工具与本地受理](agent-tools.md)

过程说明、工具调用与执行输出保留在原 rollout。Issue Chat 在对应的执行位置显示「查看执行过程」入口，默认收起。打开消息页只从 Server 读取本页需要的消息和执行引用。用户点开入口时，Web 通过共用 API 向原 Run 所属 Runner 读取这段执行过程。已加载内容可收起，展开时也可以按需刷新。原 Runner 暂不可达时，在对应入口显示读取状态，已同步的留言和交付仍可正常阅读。 [按需读取 rollout](server-runner.md#按需读取-rollout)

查看执行过程是只读操作。上级 Agent 依据持久提交的留言、业务状态变化和新交付收到唤醒。普通阶段进展沿原 Issue 保存。旧 Tend 提供默认折叠的执行步骤；一期明确把原件查询放在用户展开时。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒) · [旧执行步骤界面](https://github.com/GhostFlying/tend/blob/83bc15ae/web/src/rollout/RunRollout.tsx#L96-L102)

## 逐级上报与用户决策

各级 Agent 在自己的责任范围内处理能决定的事项。需要上级判断时，沿委派关系向直接上级上报原问题、已知事实和受影响的工作，保留原 Issue 与证据引用。上级依据自己的责任判断，必要时继续向上报。

顶层 Issue Agent 将需要用户决定的事项提交给 Router，Server 持久记录原话题、责任 Agent、问题、已有证据以及已知的选项和影响。Router 在原 Chat 提出可作答的 Attention，结合任务优先级、时限和延迟答复的影响安排提醒。用户答复由 Router 沿原决策项送回责任 Agent。依赖答案的动作等待，其余可做的工作照常推进。

派发无候选时，Router 可以根据 Server 保存的原因判断等待、调整选择或请求用户介入。相关问题沿原任务和原 Chat 关联。 [无候选时的处理讨论](agent-selection.md#无候选时的反馈与改选)

## Chat 内处理 Attention

一期优先让用户在 Chat 里完成所有问答和决策。需要用户行动的事项一旦产生便持久记录，用户可以问「还有哪些需要我处理？」。Router 从实际待办读取，每个事项能回到原话题。后续需要更直接的入口时，可以在 Chat 中增加一个可展开的待处理摘要。回复与 CLI 操作使用同一 Attention 和答复状态。

主时间线保留直接对话、约定交付、重要变化和需要提醒的具体问题。紧迫事项由 Router 主动提问。普通待办可随用户追问或适合的汇报合并带回。一次提醒可包含多个事项，每个事项各有处理状态。常规的 CI、运行和排队变化更新原工作的摘要或 Issue 详情。

## 只读通知

用户打开通知可以浏览已经发生的工作动态，例如 Issue 已开始、进入等待、恢复或结束。通知从已记录的状态变化读取时间、前后状态和工作链接。首期用文本和链接显示，无未读标记，浏览只读取已有记录。需要答复的事项通过 Chat 里的 Attention 处理。

验收时让 A 完成交付、B 产生待处理问题、多个普通 CI 和 Issue 状态变化，同时用户正在 Chat 中写 C。A 的结果只出现一次，B 能从 Chat 找到并回答。CI 变化留在关联工作，Issue 流转可从通知查看，C 的草稿、焦点和阅读位置保留。

旧 Tend 的逻辑会话、按承诺发布结果和同一个 Attention 跨入口接续提供了行为参考。首期用一条 A、B 并行、C 继续聊天的 fixture 核对归属、结果和用户注意力。 [旧主 Agent 职责](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/design.md#L105) · [按约定发布的旧设计](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/quiet-chat-publication.md#L3-L14) · [旧 Attention 接续设计](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/attention-planner.md#L23-L44)
