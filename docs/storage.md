# 数据库存储与访问边界

**已确认。** Server 数据库直接保存 Chat 与 Issue 讨论的完整消息，包括 Agent 留言，以及 Issue 当前状态。交付文件的完整原件同步到 Server 的文件存储。Codex rollout 等原生执行原件留在各自配置的独立 Codex home。Router、讨论、转交和飞书研判原件留在 Server 本机对应 home，工作 Run 原件由实际执行的 Runner 或同 Host 已接替的后继从原 Codex home 读取。Issue 及其关联记录是工作事实来源。一次业务变化与可查的原操作结果和必需待投递记录在所属实例的同一短事务提交。

数据归属遵循任务、工作区和回执契约。具体表结构随各模块实现确定。

## 旧 Tend 的实际故障

- **写入排队。** #393 的负责人事件已处理，ACK 等 SQLite 写连接 5.011 秒，事务还未开始。该 ACK 曾影响活跃执行和事件重放。另一组慢写样本中，245 次至少一秒的操作合计耗时 339.04 秒，315.40 秒在等待共享写连接。合计包含并发重叠。旧 Tend 已有独立原件目录，业务回执和其他写入仍在争用同一写连接。 [原事件 ACK 修复](https://github.com/GhostFlying/tend/pull/393) · [短断线复盘](https://github.com/GhostFlying/tend/pull/399) · [共享写连接样本](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/shared-writer-contention.md#L1-L21)
- **读取历史放大。** #257 的用量 GET 先扫描历史请求、解码和重算，再应用查询窗口。一次隔离快照对照从约 2,576 条 SQL、441 毫秒降到约 10 条、105 毫秒。看板上的 handoff 查询也曾将同一工作的历史自连接，再用 `DISTINCT` 去重。两个案例分别涉及计算放在读取路径和历史关联规模。 [用量读路径修复](https://github.com/GhostFlying/tend/pull/257) · [看板历史自连接](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/manager-delivery-query-latency.md#L5-L15)
- **原件故障传播。** #306 之前，rollout 路径别名被拒绝会挡住负责人工具和完成事件。#306 将事件持久化与归档分开，#307 从真实业务入口验证故障隔离。 [归档与事件隔离](https://github.com/GhostFlying/tend/pull/306) · [主链回归](https://github.com/GhostFlying/tend/pull/307)

## 建议保存范围

| 位置 | 保存内容 | 用途 |
| --- | --- | --- |
| **Server DB** | Chat 和 Issue 讨论的完整消息及引用，Router 的待处理输入与原生回合引用，交付文件引用和可用状态，原生记录的执行归属与引用，Host 与 Runner 登记和对应关系，Issue 当前责任和状态、顶层 Issue 的 Runner 归属、重要流转与用户决定，Attention，任务派发、输入和控制待投递记录，PR / CI 事实及证据引用、Issue 关联、外部订阅与命中记录，业务操作和同步回执。 | 读取工作现状，恢复原消息和派发，查明原操作结果。看板和通知直接读取相关当前字段和流转记录。 |
| **Server DB · `web_sessions`** | 迁移 30。随机会话 ID 的 SHA-256 摘要作为主键，另存共享凭据代次摘要、绑定的 origin 和 Unix 秒绝对到期时间。 | 重启后校验 Cookie。退出删除当前记录，启动回收旧代次和已过期记录，登录按到期索引回收过期记录。读取不续期、不写回。不保存原始共享令牌、原始 Cookie 或会话 ID。 [Web 会话契约](web-session.md) |
| **Runner DB** | 已领取任务和续作所需的输入快照，workspace 归属及独占该目录的 Task，执行和实际停妥证据，待处理输入，原生工具调用的本地操作及结果，交付原件引用，待同步记录和确认位置。 | 独立开工、重启恢复和全量对账。清理已确认的待传记录后，仍能从保存的原事实对账。 |
| **Server 文件存储** | 已从 Runner 同步的交付文件原件。 | 通过文件引用下载，Runner 离线时仍可读。 |
| **Server 本机原生存储** | Router 的 Codex session、rollout 和其他原生记录。 | 本机执行与恢复，执行详情按需读取。 |
| **Runner 交付文件存储** | 提交时固定并保存在托管 workspace 之外的交付文件原件。首期同步完成后继续保留。 | 同 Host 各代共用按文件 ID 寻址的不可变目录，增量和全量均可补回 Server 缺失的原件。 |
| **Runner 文件及原生存储** | Codex session 和 rollout、原生工具记录、长命令输出、诊断原件和 workspace 文件。 | 本地推进与恢复。退役时只保留原 thread 到 launcher/home 的索引。详情按原 Run 的 thread/turn 实时读取原件。 |
| **内存或运行日志** | 页面刷新信号、无变化的轮询、连接细节、SQL 计时与其他临时信号。 | 支持刷新和诊断。需要长期查询的联系信息可以合并保存最近一次观察。 |

首期普通读写围绕关系表中的当前状态和必要历史。Chat 与 Issue 可以分别引用同一条消息或结果。通知从持久的状态变化读取。原生记录支撑执行查证，Agent 借助保存在 Issue 上的交接信息继续工作。

## 写入与故障边界

一次业务变化所更新的当前状态、原操作结果和必需的后续待投递记录写入所属实例的同一个短事务。

| 入口 | 一次本地提交 | 提交后的回执 |
| --- | --- | --- |
| **Server 收到主 Chat** | 完整消息及 Router 的待处理输入。 | Chat 显示已收到，Router 随后读取待处理输入。 |
| **Runner 受理 Agent 工具调用** | 原生调用与原操作的关联、原操作与其待同步记录。 | 工具返回操作 ID，Runner 继续当前工作。 |
| **Server 应用本机 Router 派发** | 原消息与目标 Issue 的关联、目标 Issue 的待领取任务、原操作结果。 | 工具可以按原操作 ID 读取已提交结果。 |

Runner 本地受理与 Server 应用分别提交。持久收到事实与实际应用操作的状态各有回执。网络回执从已经提交的记录生成。若派发事务已提交而响应丢失，重传同一操作可以读回原 Issue 和任务，始终对应一次实际派发。 [旧 Tend 独立 ACK 写入复盘](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/manager-run-timeout-recovery.md#L1-L8)

批量回传有界提交，单次写事务只处理本次受影响的记录。访问模型或外部服务、复制原件、计算历史汇总以及等待原进程均在事务之外。附加统计按需单独计算，涉及写时汇总时按受影响的范围更新。

Server 可先在短事务里保存 Task 结算、停妥证明、完整留言及交付引用，引用记录当时的文件同步状态。原件通过独立的文件传输完整持久保存后，用短事务更新可用状态并记录本次交付所需的上级输入。首次交付输入沿原交付身份去重，补回历史原件时更新原引用的可用状态。下载只读取已可用的原件，文件缺失时只返回待补回。Server 独立后台任务按行号每秒核对一批文件，Runner 全量对账也按 Task 核对。两者都在写事务内复查文件尚未补回后标记待上传，不依赖 GET 触发补传。 [交付副本与全量补回](server-runner.md#交付副本与全量补回)

## 读取边界

- **Chat 和 Issue 讨论。** 主 Chat 按会话在数据库取一页完整消息。父 Issue Chat 按父 Issue 与下属子 Issue 的合并时间线在数据库取一页原留言，保留每条消息的身份与归属。再按本页 ID 批量查引用、附件信息和处理状态。执行引用用于显示折叠入口，点击后再读取对应原件。日常刷新读取新增消息和已展示消息所需的状态，历史可继续按页查阅。 [Issue Chat 合并展示](chat-routing.md#issue-chat-合并展示) · [旧 Tend 消息页读取放大](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/web-messaging-read-amplification.md#L3-L10)
- **看板。** 按当前阶段入口时间及 Issue ID 截页，再读取本页 Issue 树、PR 关联和已保存的状态摘要。入口证据随影响阶段的业务事务保存，仅对受影响的 Issue 与其父链计算。迁移 28 只初始化对比基线，未知的历史入口不补时，读取不写回。 [排序与分页契约](plans/issue-board-phases.md#入列顺序与分页)卡片直接展示链接及状态。详情按所选 Issue 分页读取执行和历史。已回传的评审材料只读原话题消息、该 Issue 的持久报告和交付关联。未被确认关联的过程附件不混入报告。原件按引用通过现有鉴权打开，已结案的历史记录不需要回填或重写。 [排序与分页契约](plans/issue-board-phases.md#入列顺序与分页)
- **交付文件与 rollout。** 已同步的交付文件从 Server 的文件存储读取。打开或刷新 rollout 详情时，Web 通过共用 API 读取实际执行实例上当前可用的原件。 [按需读取 rollout](server-runner.md#按需读取-rollout)

Runner 按自身身份读取已分配的待投递任务。建议空查询只读返回，实际分配和接收按各自的持久记录确认。心跳可合并保存最新观察。Runner 持久保存本机 Repo 登记，Server 保存可用于派发的 repo 标识与 Runner 对应关系。 [分配与投递](server-runner.md#分配与投递) · [Repo 注册](server-runner.md#repo-注册) · [旧 Tend 空领取对照](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/findings/issue-lifecycle-efficiency-performance.md#L27-L43)

归属、状态、排序和幂等身份使用可建索引的列。SQL 索引以以上具体访问路径为依据。定向 fixture 保持当前页和结果相同，只增加无关历史，核对分页内容、SQL 次数和查询计划。用 SQLite 的 `EXPLAIN QUERY PLAN` 检查查找和扫描范围。 [SQLite 查询计划](https://sqlite.org/eqp.html)

## 选型和验收

**已确认。** 一期由单个活动 Server 和每个 Runner 各自持有本地 SQLite WAL 库。WAL 支持读写并行，每个库同时只有一个写者，长读也会限制 checkpoint 的推进。旧 Tend 已使用 WAL 和单写连接，慢写样本主要耗在取写连接。新版通过有界补传和短事务缩小后台同步占用写连接的时间。 [SQLite WAL 并发与 checkpoint](https://sqlite.org/wal.html#concurrency) · [旧写连接配置](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/store/store.go#L62-L81) · [旧慢写样本](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/shared-writer-contention.md#L1-L5)

用同一组 fixture 在 devcontainer 对照日常通信与多 Runner 重连补传。补传期间读取看板、提交 Chat 和 Attention 答复，并上传交付原件。再增加历史量核对本页读取的查询次数和扫描范围。分别观察用户请求延迟、写连接排队、事务执行与提交时间，同时核对原结果、工作区可释放证据及重复回执。性能验证使用预期部署的持久化配置。若持久回执承诺在断电后仍可读回，SQLite WAL 的测试基线使用 `synchronous=FULL`。 [SQLite 提交持久性](https://sqlite.org/pragma.html#pragma_synchronous)

Server 的 Runner 同步由独立的有界队列处理，文件传输和 rollout 也各有处理入口。启用 `TEND_SQLITE_TIMING=1` 可以分别观察 Chat 收件、Attention 结算和 Runner 同步的写锁等待、事务体与提交耗时。前台连接在短事务争用期间按较短的间隔重试。 [一期补传负载结果](plans/phase-one-closeout.md#一期验收结果)

如果批次有界、读查询有索引且事务已经足够短，预期负载下仍反复发生跨 Issue 写入排队并拖慢用户的 Chat 或 Attention 写操作，再用同一负载评估 Server 切换 PostgreSQL。PostgreSQL 可以并发更新互不冲突的行，Runner 的本地存储独立选型。 [PostgreSQL 行锁](https://www.postgresql.org/docs/current/explicit-locking.html#LOCKING-ROWS)

### 普通报告的历史补录

迁移 27 新增 `report_revisions` 和 `report_revision_receipts`。在同一个短 `IMMEDIATE` 事务中，以 Issue 的 `source_seq` 和原消息校验两份普通 `issue_reports`、可选 Router 输入（若存在须同源且已成功结算）及两个原发布消息，按需补齐 `issue_report_publications`，再保存旧→新边、锁定的发布消息身份及操作回执。已有发布映射不覆盖，已有边不重写，旧报告只允许一个后继、新报告只允许一个前驱，拒绝环和已知的结构化提案候选。重传原操作返回已结算回执。普通报告链只用于 Issue 查询，不产生任何待投递记录。 [操作与查询契约](plans/report-version-backfill.md#implemented-contract)

## Issue 改名

Server 迁移 55 新增 Issue 标题日志和按 Issue 分页索引。当前标题、改名和原操作回执在同一短事务内提交。Agent 名字从原领取合同冻结，原 Run/Assignment 可沿 Runner 操作关联核对。用户重复请求包括无变更的请求沿原收件读取，未实际改名的收件不进入展示历史。现有 Issue 不回填虚构事件。 [标题契约](task-lifecycle.md#issue-标题与改名历史)

## 独立讨论

Server 迁移 32 保存独立 discussion Chat 身份、标题与来源、按稳定身份去重的引用、草稿回合及转交回链。原消息、输入、原生尝试和回复沿原 Chat 表保存。草稿使用同一个持久收件与回合队列，和可见消息分页分开。显式转交的操作身份、已确认请求、主 Chat 消息、Router 待处理输入和双向回链在一个短事务内提交，响应丢失沿同一操作重读。GitHub 读取和 Runner inspection 在事务外执行。工作目录本身不进入引用或转交 SQLite 记录，当前可用性按操作重新确认。

Server 迁移 34 扩展 Issue 时间线。原 `seq`、消息 ID、引用、索引、去重键和批准事件的分页位置均保留。派发的可选来源名称作为消息快照保存。接手事件以唯一的 assignment 外键和原接受回执在同一事务提交，沿原时间线分页，不加入 Runner 投递。此前的数据库没有可供回填的确认时刻。旧 Server 不支持新版数据库，读取含 `takeover` 类型的消息页需要同步更新 CLI。

迁移 33 在原讨论行新增归档标记，既有讨论默认未归档，两个列表按各自归档状态和活动时间读取。归档与恢复只更新该标记。旧默认标题「新讨论」在原创建未提供标题、当前标题仍相同且没有可确认的改名证据时改为「未命名讨论」，不改变原请求、时间或身份。活动时间变化但没有普通用户消息的旧记录可确认改名，保留其标题。旧版没有改名日志。同一秒内改为完全相同的旧默认值，或存在普通消息活动时，不能仅凭旧数据可靠地区分该文字是系统默认还是用户指定。

迁移 42 在讨论行记录 `title_source`，区分未填写、首条输入和手动标题。接受首条真实用户 Chat 消息的事务归一化并保存完整标题，消息重放不会改写。升级只回填旧版仍为「未命名讨论」且原创建无显式标题、没有可识别改名证据的记录。旧版本不记录独立改名事件。旧普通用户消息插入和活动更新时间可能跨一秒，因而有消息时的 0–1 秒差不算改名证据。同值改名与普通用户消息相邻或之后又发了消息时，历史记录不一定能区分手动同名标题和默认标题。升级后手动标记没有此歧义。旧 Server 不支持迁移后的数据库。

迁移 42 也保存一键转交的原引用快照、请求身份、失败代次、登记时的原生 thread 与已结算可见尝试前沿。每次显式重试保留原队列输入和原尝试记录，跳过已结算的尝试。边界外的原生历史不会直接送给模型，恢复只使用安全前缀和边界前持久消息。失败不写主 Chat，成功在同一事务写自然语言正文及转交业务回执。持久的自动转交身份用于选择主 Chat 的只读隔离接收回合、阻止代答普通 Attention 和授权写入。隔离回合不写普通 Router thread 绑定。SQLite 不保存模型认证信息。

迁移 50 为普通讨论新增 `discussion_progress`。只保存已核实的自然语言进展，沿 `router_attempts` 的原身份和原生 item ID 去重，不修改 Chat 消息或输入结算。消息页在所选讨论的页范围内批量读取，不因 GET 读取原生历史或写库。 [回合进展与恢复](plans/discussion-turn-progress.md)

## Issue 树 Token 用量

Server 迁移 38 为已接受的原生 Run Start 初始化覆盖分母，不回填历史 Token。Runner 将原生逐请求观察和独立同步确认持久保留。Server 校验原 Runner、assignment、Task、Run 和原生回合后，以 SQLite 短事务接收原始键及冲突证据，按受影响的 Run、直属 Issue 与祖先更新持久小计。未核验归属的用量不进入小计。用户读取只访问所选 Issue 的闭包和已保存的直属/子树小计，不读原生原件或逐请求表。具体语义、性能证据及混代边界见 [Issue 树 Token 用量](plans/issue-token-usage.md)。迁移 41 加入按需回溯作业、来源冲突审计和影子投影，不遍历请求或原件；历史回传与显式重建见 [历史 Token 用量回溯与重建](plans/token-usage-backfill.md)。 迁移 49 只在升级时从已保存的请求补齐所选型号的小计，不重扫原件或改计量账本。全局与 Issue API 延用持久汇总和离线价格快照。

迁移 54 与迁移 52 共用价格投影重建，从已保存的请求账本补齐可计价请求计数。只更新派生模型小计，不改请求账本、原始用量或覆盖分母，不创建历史回填任务。正常请求仍只读取汇总。 [按原请求展示估算覆盖](plans/request-pricing-coverage.md)

迁移 51 只为模型 attempt 增加新研判的原 Codex home 与本次所选模型，不迁移原件或推断旧型号。

## 飞书建议

Server 迁移 46 按固定飞书用户身份和来源消息 ID 记去重关系，按话题维护有版本的证据，并分别保存采集检查点、研判与显式用户操作。扫描的 `running` 只是展示状态，不参与领用或互斥。CLI 和模型进程都在短 SQLite 事务外。完整分页窗口将来源、建议与检查点原子写入。空窗口同样推进，重启和失败从最近提交的窗口恢复。长离线分段追赶，来源不完整时留下可见缺口。关系和权限仍由 Tend 保存，OAuth 仍归本机飞书 CLI。

短期采集保留七天，由 Server 独立清理，不依赖飞书身份或扫描成功。上下文每轮有界分页，未完成时保留原窗口和游标。显式引用和快转保存所选版本的不可变快照，名称、覆盖和研判变化与源材料版本分开，随后新材料不会改写旧转交。已提交的附入请求沿原快照返回回执，不覆盖后续替换或移除。上下文证实临时分开的普通回复属于原话题后，来源关联原子移动到原建议，旧 ID 仍指向原建议，已登记动作继续使用自己的旧 ID 和版本。快转桥接、快转请求与引用在同一事务接受；按建议和版本查证原回执。飞书来源只作外部事实，不进入真实用户原话索引。

## 维护建议

Server 迁移 53 保存按异常类型和原对象唯一的建议、证据版本与人工忽略标记。飞书实际游标推进与完整扫描证明随来源窗口一同提交，不回填未观察到的旧进展。采样按已失败与待复核的 Task 键分别分页。Web/CLI 读已存建议，不触发探测或遍历 Run 历史。显式加入的说明以建议、版本、目标 Chat 去重，说明与回执在同一事务写入，不加入执行队列。 [首期探测和处置](plans/maintenance-suggestions.md)
