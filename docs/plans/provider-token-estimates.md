# 供应商用量语义与参考费用

与 #146 独立。修复 Tend 自身的 OpenRouter 离线快照估算，合入 origin/main 即交付。不部署、不运行线上统计或回填。沿用原请求身份、去重、模型证据和当前快照数值。

## 依据与实现

- [sub2api e52fca699](https://github.com/Wei-Shaw/sub2api/tree/e52fca6996aa14a657f775de7cea13a19f746920) 的 `normalizeAnthropicCompatiblePromptUsage` 优先采用 DeepSeek 明示 miss，并区分已互斥的输入分项。其 `openAIUsageFromGJSON` 优先使用标准缓存明细，再看供应商别名。计价层也区分明确零写价与未配置费用。只借鉴口径，不引入其价格、费率倍率或零值回退。
- [AxonHub b8bb3421](https://github.com/looplj/axonhub/tree/b8bb3421e1fe946cfc4f0d86e8412098738f2793) 的 Responses 归一化保留读、写和推理明细。成本层从 inclusive 输入扣除另计的缓存项，输出不再叠加 reasoning。其 Anthropic 适配器先把原生互斥输入转成 inclusive 再使用这一公式。Tend 此次仍估算快照内的 DeepSeek/OpenAI 模型，不扩充 Anthropic 采集或价格。
- 原账本保存明确 miss（包括零），不将其与推导 miss 混用。不补写原始 null，旧兼容记录亦不注入可选零值。DeepSeek 按 hit/miss 双分项计费，缺明示 miss 时只在 inclusive 总量与 hit 均可核验时求差。GPT/Responses 使用 inclusive 输入，扣除已知另计项。明细不足时，将无法细分的剩余量单列为普通输入价基准，解释未观测读折扣及可能的另计写价调整。明确零写价、已含在普通输入、缺写单价分别处理。reasoning 不重复收费。
- 每请求先检查输入分区和价格档，再聚合。保留未匹配模型、缺模型、未知档及 UTC 条件原因。全局/来源/Issue 共用明细，显示计入项目、已覆盖请求数、未知项和基准假设。升级一次性从原账本重建派生缓存，普通读不扫描账本或访问价格 API。

## 验证与交付

从用户给定的 1,211,884 输入、806,656 hit、104,232 输出生成 180 条隔离记录，核算 405,228 miss 与 $0.1038973152。Runner 核对 $4.501813372，若无可靠历史分项，以清楚标记的合成分项复算该目标金额，不假称读取了线上账本。覆盖明示/推导/未知计量、0/null、未配置单价、输入和输出不重复、逐请求档位及历史缓存迁移/重建。用实际 Server 数据渲染全局和 Issue 页面并提交截图、可复核验收数据、验证结论和完整远端 SHA。

## 首轮审阅修订

- Responses 与 Chat Completions 的标准缓存读字段（含明确的零）优先于 DeepSeek 顶层 hit 别名，明示 miss 独立保留。用同请求标识重放混合字段，验证别名不会引发隔离，标准明细真的冲突仍会隔离。
- 旧账本有可能已保存归一化默认零。只在共享计费入口按 provider provenance 将未核验的可选零视为未知，迁移、重建与增减重放一致。历史原账本与原始字段汇总不被迁移改写，精确上报的零仍用于计价。
- 已有 main 占用 schema 51 持久化 Feishu 原件索引。本变更只接续使用下一版本重建价格投影，保留该迁移和 #146 的独立改动。
