# Codebase MR 评审评论必送

## 目标和边界

把已直接关联 Codebase MR 的新评审评论作为 required 外部事实送到每个关联 Issue 的当前责任 Agent。父子 Issue 各自收件，事实不授予任何代码、MR 或 Issue 状态写入权限。继续观察已合并和已关闭的 MR。保持 GitHub 观测、路由、订阅和等待语义不变。本次只交付源码，不部署。

## 采集和基线

复用 Server 的 `--codebase-cli` 与有界子进程读取，不继承 Tend 令牌，禁止 CLI 自动升级。用现有只读 `mr get` 核对仓库和 MR 标识，再以固定参数调用原始 API 的 `ListThreads`、`ListTimelineEvents`。不接受任意 action。两份原始结果中的列表都必须显式存在，拒绝错误、缺失、重复身份或不完整结果。仅采集发布的线程评论及确有正文的评审备注。对正文及定位保留来源证据，不推断缺失值。

在 CLI/API、Runner 关联和分支补查路径写入直接关联前尝试读取完整 ID 集合，不在 SQLite 写锁内访问外部系统。成功时以该次紧邻关联的快照为基线，不用秒级时间比较。同一远端快照与本地提交没有跨系统原子边界，快照读取之后到提交之间出现的评论视为新评论。重复关联不能重置基线。关联时读取失败仍保留关联，标记待建立基线；首次成功时以关联秒为下界，宁可送达边界同秒的旧评论也不丢同秒新评论。只在失败补记路径比较 Tend 与 Codebase 的时钟，明显时钟偏差可能影响这一判定。旧库中已有的关联以首次完整观测为基线，不回放历史。失败观测不改变上次成功记录或已见集合。

## 存储和投递

仅追加 SQLite 评论观察、每关联基线/已见记录和每 Issue 的合并窗口。以规范 MR 和带来源类型的稳定 ID 去重；每个 Issue 保留独立收件身份。首条待投事实起算固定 5 分钟，关联 Issue 的后续评论加入同一窗口；尽量合入该实际目标已存在的外部窗口。窗口到期后在同一事务中入持久 Issue 输入队列，Runner 继续沿通常的同步和 Run 消费回执确认。无责任 Task 时持久排队、回退重试，并可查询阻塞原因。旧 Runner 不接收未知类型，输入留在原目标队列，直到支持该类型的 Runner 接续。用户消息和 Attention 答复继续走独立的即时路径。

## 验证

- 定向 Rust 测试覆盖原始列表形状、显式空列表、缺失/截断、事件过滤、同秒 ID 基线与编辑、失败补记、旧库升级和非支持 Runner。
- 固定 CLI 和 App Server fixture 配合真实 Server、CLI 和 Runner 验证直接关联、多个 Issue、终态 MR、5 分钟批次、断线和重启、权限失败与超时。用隔离 SQLite 加速窗口并核对真实 due_at、Run 消费及不重投。
- 运行 workspace Rust 测试和构建，以及已有 Codebase 关联、GitHub 投递、等待和协议窗口 fixture。只用 `mr comment list` 对真实 Codebase 做只读形状抽查。


### 复跑命令

```bash
cargo test --workspace --locked --offline
cargo test -p tend-server codebase --locked --offline
cargo build --workspace --locked --offline
bins=(target/debug/tend-server target/debug/tend target/debug/tend-runner)
for scenario in codebase_comments codebase_pulls external_routing external_wait protocol_window; do
  python3 -B "tests/e2e/${scenario}.py" "${bins[@]}"
done
```

### 独立审阅后的修订

- 每次成功的关联预检也给其开始前已关联的 Issue 结算同一份事实，防止评论在下一轮轮询前删除而漏投。新的关联始终用自己的 ID 集合，不接收先前已开始的快照。
- 评论加入已存在的普通窗口时，观察事务直接暂存同一 `wake_at` 的收件项。加入 GitHub 窗口时，保存准确的伴随 Task，在该 GitHub 批次的释放事务内共同发布，不靠相同到期时间推断原子性。
- 已停止或未领取的 Task 不由评论自动重启。新的责任分配确认后，未消费评论沿同一 Issue 重挂，保留输入身份和批次时间。补充部分 HTTP 响应、相邻 GitHub 窗口、普通窗口可见性、预检后评论删除和停止后重新分配的回归用例。

复审补充两条围栏。Codebase 早暂存、到期释放和同步都检查 Issue 取消及原 Task 未结算的停止意图，继续保留事实和未消费的输入身份，重开后才可沿正常责任路径补投。每次读取远端之前，在同一个 SQLite 读快照内记录可见的关联上界及已经提交基线的关联身份。已开始但当时看不到旧库基线的其他查询，不能凭相同毫秒时间跨过这条边界。

取消后待接续的评论批次仍以原 Issue 和截止时间为边界。若同批已有暂存成员而其余成员尚待释放，同步暂不返回已暂存的 Codebase 成员。释放事务补齐整批并删除待释放记录后，Runner 才能同时看见它们。这不改变普通输入、GitHub 事实或停止控制的可见性。
