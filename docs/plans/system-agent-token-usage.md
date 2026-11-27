# 全系统 Agent 请求用量

目标是将所有 Tend 发起的原生 Agent 回合纳入全局用量。保留 Issue 直属和子树读法，区分 Runner、主 Chat Router、讨论回复、讨论转交总结、飞书研判。机械扫描、历史读取、fork、通知重放本身不是模型请求。交付止于核实远端 main，不部署，不在生产执行回填。

先盘点 Codex 客户端全部 native turn 入口、各自原生身份及 rollout 保留条件。以原 thread、turn、上游 response 为键，不用业务尝试 ID 或累计用量去推断请求。处理同 thread 多次 turn、原生重试、重放、复制历史、实时和历史交叠。复用 Runner 可信归属与原有 Issue 投影；Server 侧单独保存原生归属和请求事实，在写路径更新缓存。全局合计不重复合并 Issue 子树。证据不足的请求、模型和价格保持未知，并显示覆盖缺口。

显式低频回填复用 Runner 原有全量作业，补充 Server 已知原生 turn 的回填，保留覆盖和缺口。只读取已保留的本机证据，不启动模型、不扫描任意用户会话入账、不在普通页面或启动时执行原始证据遍历。区分旧版未记录、活跃前缀、不可用原件与成功扫描；不承诺供应商计费完整性。

在隔离进程 fixture 中验证每个实际调用入口、重试/重放、客户端和存储重启、共享历史、全量回填重跑和缺证据。核对全局缓存与 Issue 小计、查询路径和 Web 桌面/窄屏，交付最终代码实际渲染图。格式、定向 Rust 和 Web 检查、release 及相关 E2E 通过后，对实际候选独立审阅，按仓库门禁快进合入，核实远端完整 SHA。

### 入口盘点

- Runner 统一通过 execution 驱动 Issue Owner、Work、Review 与接续，原生协作子 Agent 另有独立 thread，不应复用父 turn 身份。
- Server 通过 Router 驱动主 Chat（包括正常转交），Discussion 分别驱动普通/草稿回复、快速转交总结和旧转交的隔离接收。飞书扫描与飞书研判分离，只有后者启用隐私隔离的 ephemeral turn。
- App Server 的 `rawResponse/completed` 是 response 键控的请求证据，即使用量缺失也能证明已完成响应。已有 rollout 只用于逐请求兼容采集和历史明确回填，绝不使用 `thread/tokenUsage/updated` 单独推算。飞书继续使用 ephemeral，不以计量为由保留材料。

## 已实现的读写口径

| 来源 | 可信执行事实 | 归属 |
| --- | --- | --- |
| Issue Agent | 原 Runner Started 及本地持久的逐请求事件，包含同根 turn 的原生子 Agent | 原 Run / 原 Issue |
| 主 Chat Router | 开始并确认的 Server 尝试，包括接收正常转交 | Router |
| 讨论回复 | 普通讨论回合 | Discussion |
| 讨论转交总结 | 自动草稿或一键转交的总结回合 | Discussion summary |
| 隔离转交接收 | 旧一键转交的隔离接收回合 | Handoff receiver |
| 飞书研判 | 隔离、ephemeral 的模型回合 | Feishu assessor |

- 每个请求使用原 thread、turn 和上游 response ID。保持原始回合，业务重试若真正发出新请求则新增记录。复制的旧历史、重复帧、恢复读取和二次同步不产生新请求。只有 native turn 生命周期明确给出同一根 turn 的子回合，或原件记录给出同 session 和根 turn，才允许纳入子 Agent。原生子 Agent 的用量归原 Run、原 Issue，只在全局 Runner 行汇总一次。反序校验时用原 assignment 和根 turn，不用目前的 Owner 或路由。
- 开启原生 `experimentalRawEvents` 并屏蔽原始消息通知。计量只消费响应完成事实，不保存 raw payload、提问、回复或账户元数据。旧 App Server 或已恢复的线程未发逐请求完成事件时，沿已返回的原 rollout 路径，以追加的原生 response 键佐证同 turn 的 token 通知。未匹配的累计量、压缩重估或窗口事件不计量。
- Server 原生尝试、实际 turn、逐请求证据和来源小计持久化。多个尝试引用同个原生 turn 只沿首次确认的来源入账。相同请求也在 Issue 台账时优先归 Issue，Server 影子副本不贡献用量。两侧任一写入顺序、Runner 补录撤回和投影重建均不把共享键重复计入。
- 根页 `total` 汇总现有 Issue 直属的全局缓存和固定数量的 Server 来源缓存。`sources` 按入口给出请求、已确认覆盖、共享项及缺口。总计对象的 Run 分母仍只代表 Issue Run。下钻只读 Issue 直属和子树缓存，无 `total` 或 `sources` 字段。数据不是供应商账单。缺字段或无可用字段保留未知，已知字段的小计不代表完整。有供应商原始字段时，原生标准化的缺省零不能补齐供应商未提供的字段。只有原件标准化值的历史请求无法反推原始字段是否省略。实际响应模型有明确证据才使用公开费率估算，回合配置或回合上下文不是实际模型，不用账单估算替代实付。
- Server schema 48 只从原生命周期元数据登记历史覆盖分母，不打开 rollout，也不自动跑回填。独立的飞书机械扫描、任何历史作业本身、查看用量、分支展开都不启动模型。

## 历史与覆盖

`tend token-usage backfill --all --job <id>` 同时登记既有 Runner 页式作业以及 Server 侧按尝试扫描的队列。Server 只在收到显式全量受理或恢复其已持久的未完项时读原件，使用已绑定路径或者当前配置的本机 Codex home，按已核实原 thread、turn 获取有效逐请求记录。子 Agent 仍保留自己的 native 键，只取原 session 和根 turn 能对应的记录。历史副本补齐相容字段，且不把历史中没有出现的实时键扣掉。Server 请求写入按批推进。普通 GET 不查询作业状态或历史表。

`tend token-usage status <id>` 保留原有 Runner 分页，在 `server` 字段返回按来源归组的已扫描、进行中前缀和缺口（例如原件缺失、未核实 turn、内容不相容和 ephemeral）。飞书此前的临时会话没有可识别的请求历史，启用采集前的覆盖不可反推。保留同一 job ID 用于重试已受理作业，用新 ID 可重扫更新后的证据。缩小范围的回填和现有显式重建仍只作用于 Runner 和 Issue 投影，后者不读原件。

## 边界

- 请求已发送但没有可核实响应 ID、断线尾部、无法提供逐请求证据的旧实现及没有留存的 ephemeral 会话不能从累计值补算。兼容旧 Server 时，它不会把新格式的子 turn 归到父 turn。Runner 本地原记录可供新版重新对账，留存的原件也可显式回填。
- 运行时可能自己触发子 Agent 和压缩，只有原生事件或留存记录能核实的响应才入账。恢复的线程若不支持 raw 完成通知，缺失逐请求内容就保留未知。测试以隔离协议 fixture 和无真实账单的本地 Responses 模拟端点验证，不读取用户 Codex 会话。

### 独立审阅后的增量验收

- 统一实时、历史和显式重建对无用量响应的身份计数，验证同一已知重叠键只归 Issue，且重建前后总计不变。
- 在 Runner 回填页上增加原生子 Agent 覆盖回执，旧读者可补根记录但明确呈现子 Agent 覆盖缺口。
- 兼容及历史只存在标准化计数时，不把无法区分的可选默认零解释为供应商明确零。补用真实 Codex 和本地 Responses fixture 验证 Router 恢复及飞书 ephemeral 路径。
- 审计保留子响应完整原生键，独立保留 Run 的根 thread/turn，同响应 ID 的不同子 turn 不会合并审计条目。

原生验证可执行 `python3 -B tests/e2e/system_agent_token_usage_native.py <server> <cli> <codex> <scratch>`，只使用隔离 Codex home、合成飞书材料和回环 Responses 服务。上一版 Runner 对照可用 `system_agent_token_usage.py --legacy-runner <旧 Runner>`。

### 混版本边界复审

- 在校验原 Runner/Run/turn 后统一解释计量来源。旧 Runner 的标准化可选默认零在直接写入和历史键上的迟到实时合并均视为未知。供应商原始字段证明的零不受影响。迁移不扫描或改写历史请求。
- 上游 Server 未声明子覆盖协议时，根回合用旧版原件读取、分页和摘要，避免同一旧作业在更换 Runner 二进制时误报原件变化。隔离验证同库、同原件、同作业的跨 32 键分页续接。

`python3 -B tests/e2e/token_usage_mixed_versions.py <当前 Server> <旧 Server> <CLI> <当前 Runner> <旧 Runner>` 使用隔离的进程和合成原件验证双向混版。旧 Server 测试在固定第 1 页已提交后更换 Runner，断言原 job、原摘要和原件字节保持不变。
