import { useState } from "react";
import type {
  TokenBreakdown,
  TokenEstimate,
  TokenModelRow,
  TokenPriceComponent,
} from "./api";
import { fieldNote, format, tokens } from "./TokenUsageSummary";
import "./token-model-usage.css";

const feeComponents = [
  [0, "普通输入"],
  [1, "缓存读取"],
  [2, "缓存写入"],
  [3, "输出"],
] as const;
const lineFields = [
  ["cached_input_tokens", "缓存读取"],
  ["cache_write_input_tokens", "缓存写入"],
  ["output_tokens", "输出"],
] as const;
const rawFields = [
  ["input_tokens", "输入"],
  ["cached_input_tokens", "缓存读取"],
  ["cache_write_input_tokens", "缓存写入"],
  ["output_tokens", "输出"],
  ["reasoning_output_tokens", "推理输出"],
] as const;

function Amount({ usd }: { usd: string }) {
  const [whole, fraction] = usd.split(".");
  return (
    <bdi className="model-cost">
      ${format.format(BigInt(whole))}
      {fraction ? `.${fraction}` : ""}
    </bdi>
  );
}

export function EstimateValue({
  estimate,
  requests,
}: {
  estimate: TokenEstimate;
  requests: number;
}) {
  if (requests === 0) return null;
  const unpriced = estimate.unpriced_requests ?? 0;
  return (
    <span className="model-estimate-values">
      {estimate.usd !== null && <Amount usd={estimate.usd} />}
      {unpriced > 0 && (
        <small className="detail-muted model-unpriced-count">
          {format.format(unpriced)} 条请求未计价
        </small>
      )}
      {estimate.unpriced_requests === undefined && estimate.usd === null && (
        <small className="detail-muted">费用未知</small>
      )}
    </span>
  );
}

function ModelDetails({ row }: { row: TokenModelRow }) {
  const price = row.price;
  const explicit = row.request_level_selected_model_requests ?? 0;
  const turn = (row.selected_model_requests ?? 0) - explicit;
  const actual = row.response_model_requests;
  return (
    <div className="model-detail">
      {row.model !== null && (
        <p className="detail-muted">
          模型依据
          {explicit > 0 && ` · 原生请求所选 ${format.format(explicit)} 条`}
          {turn > 0 && ` · 原生回合所选 ${format.format(turn)} 条`}
          {actual > 0 && ` · 逐响应确认 ${format.format(actual)} 条`}
        </p>
      )}
      <dl className="model-raw">
        {rawFields.map(([field, label]) => (
          <div key={field}>
            <dt>{label}</dt>
            <dd>
              <bdi>{tokens(row[field])}</bdi>
            </dd>
            {fieldNote(row[field], row) && (
              <dd className="detail-muted">{fieldNote(row[field], row)}</dd>
            )}
          </div>
        ))}
      </dl>
      {!price ? (
        <p className="detail-muted">
          {row.model !== null
            ? "价格快照没有精确匹配的条目，未估算该模型。"
            : "请求缺少可归属的原生模型证据，或模型证据不一致。"}
        </p>
      ) : (
        <>
          <p className="detail-muted">
            <a
              href={`https://openrouter.ai/${price.openrouter_id}`}
              target="_blank"
              rel="noreferrer"
            >
              {price.openrouter_id}
            </a>{" "}
            · {price.snapshot_date} 公示价
            {price.long_input_above !== null &&
              ` · 单次总输入 > ${format.format(price.long_input_above)} 按长上下文档`}
          </p>
          {price.unsupported && (
            <p className="detail-muted">
              {price.unsupported === "utc_schedule"
                ? "公示价按 UTC 时段变化。本版不选择时段费率，也不使用回填入库时间。"
                : "快照包含未支持的覆盖条件，无法确认适用费率。"}
            </p>
          )}
          <p className="detail-muted">
            {price.input_billing === "hit_miss"
              ? "DeepSeek 按命中与未命中双价计算，不另计缓存写入。未上报 miss 时，由已知总输入减去 hit。"
              : price.input_billing === "included_write"
                ? "缓存写入已含在普通输入中，不单独收费。"
                : "已知另计的缓存写入从普通输入中扣除，单独按写价估算。"}{" "}
            推理是输出子集，不重复计费。
          </p>
          {!price.unsupported &&
            price.tiers.map((tier) => {
              const notes = tier.notes ?? {};
              const writeRate = tier.components[2].usd_per_million;
              const inputRate = tier.components[0].usd_per_million;
              const writeAdjustment =
                writeRate === null || inputRate === null
                  ? "写入调整额未知"
                  : Number(writeRate) > Number(inputRate)
                    ? "可能有未计入的写入溢价"
                    : Number(writeRate) < Number(inputRate)
                      ? "基准可能高于按写价拆分后的金额"
                      : "该档写价与普通输入价相同";
              return (
                <div className="model-rate-tier" key={tier.tier}>
                  <p>
                    {tier.tier === "long"
                      ? "长上下文"
                      : tier.tier === "unknown"
                        ? "输入缺失 · 无法确认费率档"
                        : "标准费率"}{" "}
                    <span className="detail-muted">
                      · {format.format(tier.requests)} 条请求
                    </span>
                  </p>
                  {tier.baseline_input && (
                    <p className="detail-muted">
                      已知计价项扣除后，未细分的输入余额按普通输入价作基准。
                      {notes.read_discount_unobserved !== undefined &&
                        ` ${format.format(notes.read_discount_unobserved)} 条的缓存读折扣未观测。`}
                      {notes.write_adjustment_unobserved !== undefined &&
                        ` ${format.format(notes.write_adjustment_unobserved)} 条缺写入明细，${writeAdjustment}。`}
                    </p>
                  )}
                  {notes.conflicting_input !== undefined && (
                    <p className="detail-muted">
                      {format.format(notes.conflicting_input)}{" "}
                      条的输入分项与总量冲突。
                      {price.input_billing === "hit_miss"
                        ? "有明示 miss 时采用明示分项，不使用冲突的总量推导。"
                        : "未估算这些请求的输入项。"}
                    </p>
                  )}
                  {notes.unexpected_write !== undefined && (
                    <p className="detail-muted">
                      {format.format(notes.unexpected_write)}{" "}
                      条带写入明细，原计量保留，不加收写入费。
                    </p>
                  )}
                  <dl className="model-fee-items">
                    {feeComponents.map(([index, label]) =>
                      index === 2 &&
                      (price.input_billing === "hit_miss" ||
                        price.input_billing === "included_write") ? null : (
                        <FeeItem
                          key={label}
                          label={
                            price.input_billing === "hit_miss" && index === 0
                              ? "未命中输入（miss）"
                              : price.input_billing === "hit_miss" &&
                                  index === 1
                                ? "缓存读取（hit）"
                                : label
                          }
                          component={tier.components[index]}
                          requests={tier.requests}
                        />
                      ),
                    )}
                    {tier.baseline_input && (
                      <FeeItem
                        label="未细分输入（基准）"
                        component={tier.baseline_input}
                        requests={tier.baseline_input.reported_requests}
                      />
                    )}
                  </dl>
                </div>
              );
            })}
        </>
      )}
    </div>
  );
}

function FeeItem({
  label,
  component,
  requests,
}: {
  label: string;
  component: TokenPriceComponent;
  requests: number;
}) {
  const priced = component.priced_requests;
  const missingQuantity = requests - component.reported_requests;
  const missingPrice = component.reported_requests - (priced ?? 0);
  const omitted = [
    missingQuantity > 0 &&
      `${format.format(missingQuantity)} 条数量缺失或未分拆`,
    missingPrice > 0 && `${format.format(missingPrice)} 条无适用费率`,
  ].filter(Boolean);
  return (
    <div>
      <dt>{label}</dt>
      <dd className="detail-muted">
        {component.usd_per_million === null
          ? "无适用单价"
          : `$${component.usd_per_million} / 百万 Token`}
      </dd>
      <dd>
        <bdi>{tokens(component)}</bdi> Token
      </dd>
      <dd className="detail-muted">
        {priced !== undefined
          ? `已计 ${format.format(priced)} / ${format.format(requests)} 条`
          : (fieldNote(component, { reported_requests: requests }) ?? "")}
      </dd>
      {priced !== undefined && omitted.length > 0 && (
        <dd className="detail-muted">
          未计 {format.format(requests - priced)} 条 · {omitted.join(" · ")}
        </dd>
      )}
      {priced !== undefined &&
        priced > 0 &&
        component.usd_per_million === null && (
          <dd className="detail-muted">只计已知为零的数量，单价仍未知</dd>
        )}
      {component.estimate.usd !== null && (
        <dd>
          <Amount usd={component.estimate.usd} />
        </dd>
      )}
    </div>
  );
}

function ModelRow({ row }: { row: TokenModelRow }) {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="model-row"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>
        <span className="model-name">
          <strong>{row.model ?? "模型未知"}</strong>
          <span className="detail-muted model-evidence">
            {format.format(row.reported_requests)} 条请求
          </span>
        </span>
        <span className="model-row-tokens">
          <bdi>{tokens(row.total_tokens)}</bdi> Token
        </span>
        <span className="model-row-estimate">
          <EstimateValue
            estimate={row.estimate}
            requests={row.reported_requests}
          />
        </span>
        <span className="model-line-fields">
          {lineFields.map(([field, label]) => {
            const value = row[field];
            if (
              field === "cache_write_input_tokens" &&
              value.reported_requests === 0
            )
              return null;
            const note = fieldNote(value, row);
            return (
              <span className="model-line-field" key={field} title={note}>
                <span className="detail-muted">{label}</span>
                <bdi>{value.tokens === null ? "未知" : tokens(value)}</bdi>
                {value.tokens !== null && note && (
                  <small className="detail-muted">部分</small>
                )}
              </span>
            );
          })}
        </span>
      </summary>
      {open && <ModelDetails row={row} />}
    </details>
  );
}

export function TokenModelUsage({
  usage,
  label,
  compact = false,
}: {
  usage: TokenBreakdown;
  label: string;
  compact?: boolean;
}) {
  if (!usage.estimate) return null;
  return (
    <section
      className={`model-usage${compact ? " model-usage-compact" : ""}`}
      aria-label={label}
    >
      <h4>
        <span>{label}</span>
        {usage.reported_requests > 0 && (
          <span className="model-usage-cost">
            <EstimateValue
              estimate={usage.estimate}
              requests={usage.reported_requests}
            />
          </span>
        )}
      </h4>
      {!compact && (
        <p className="detail-muted model-method">
          按{" "}
          <a
            href="https://openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties"
            target="_blank"
            rel="noreferrer"
          >
            OpenRouter 公示标准价
          </a>{" "}
          对已上报请求的 Token
          估算，不是实际账单。按执行时所选模型，能确认具体请求的实际型号时用该型号。
          费用计入有适用单价的分项，缓存明细不足时可能包含普通输入价基准。
          展开查看各项覆盖和未计原因。不含未采集用量、工具费和平台费用。
        </p>
      )}
      {(usage.models?.length ?? 0) === 0 ? (
        <p className="detail-muted model-method">没有可用于模型分组的请求。</p>
      ) : (
        <ul className="model-list" aria-label="各模型用量">
          {usage.models?.map((row) => (
            <li key={row.model ?? ""}>
              <ModelRow row={row} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
