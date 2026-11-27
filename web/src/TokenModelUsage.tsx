import { useState } from "react";
import type { TokenBreakdown, TokenEstimate, TokenModelRow } from "./api";
import { fieldNote, format, tokens } from "./TokenUsageSummary";
import "./token-model-usage.css";

const feeComponents = [
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

export function EstimateValue({ estimate }: { estimate?: TokenEstimate }) {
  if (!estimate || estimate.usd === null) return <>费用未知</>;
  const [whole, fraction] = estimate.usd.split(".");
  return (
    <>
      <bdi className="model-cost">
        ${format.format(BigInt(whole))}
        {fraction ? `.${fraction}` : ""}
      </bdi>
      {estimate.partial && <small className="model-partial">部分估算</small>}
    </>
  );
}

function modelGap(row: TokenModelRow) {
  if (row.model === null) return "缺少可归属的模型证据";
  if (!row.price) return "无匹配的 OpenRouter 公示价";
  if (row.price.unsupported === "utc_schedule") return "不支持 UTC 时段价";
  if (row.price.unsupported === "other_conditions") return "不支持该价格条件";
  if (row.estimate.partial || row.estimate_gap === "incomplete_components")
    return "部分计量或费率缺失";
}

function ModelDetails({ row }: { row: TokenModelRow }) {
  const price = row.price;
  const explicit = row.request_level_selected_model_requests ?? 0;
  const turn = (row.selected_model_requests ?? 0) - explicit;
  const actual = row.response_model_requests;
  const missingFresh = price?.tiers.some(
    (tier) => tier.components[0].reported_requests < tier.requests,
  );
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
          {missingFresh && (
            <p className="detail-muted">
              缺少完整输入或缓存读写计量的请求，无法确定其非缓存输入费用。
            </p>
          )}
          {price.tiers.map((tier) => (
            <div className="model-rate-tier" key={tier.tier}>
              <p>
                {tier.tier === "long"
                  ? "长上下文"
                  : tier.tier === "unknown"
                    ? "输入缺失 · 无法确认费率档"
                    : tier.tier === "unsupported"
                      ? "条件费率未估算"
                      : "标准费率"}{" "}
                <span className="detail-muted">
                  · {format.format(tier.requests)} 条请求
                </span>
              </p>
              <dl className="model-fee-items">
                {feeComponents.map(([index, label]) => {
                  const component = tier.components[index];
                  return (
                    <div key={label}>
                      <dt>{label}</dt>
                      <dd className="detail-muted">
                        {component.usd_per_million === null
                          ? "无适用单价"
                          : `$${component.usd_per_million} / 百万 Token`}
                      </dd>
                      <dd>
                        <bdi>{tokens(component)}</bdi> Token
                      </dd>
                      {fieldNote(component, {
                        reported_requests: tier.requests,
                      }) && (
                        <dd className="detail-muted">
                          {fieldNote(component, {
                            reported_requests: tier.requests,
                          })}
                        </dd>
                      )}
                      <dd>
                        <EstimateValue estimate={component.estimate} />
                      </dd>
                    </div>
                  );
                })}
              </dl>
            </div>
          ))}
        </>
      )}
    </div>
  );
}

function ModelRow({ row }: { row: TokenModelRow }) {
  const [open, setOpen] = useState(false);
  const gap = row.estimate.usd === null ? modelGap(row) : undefined;
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
            {gap && ` · ${gap}`}
          </span>
        </span>
        <span className="model-row-tokens">
          <bdi>{tokens(row.total_tokens)}</bdi> Token
        </span>
        <span className="model-row-estimate">
          <EstimateValue estimate={row.estimate} />
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
        <span className="model-usage-cost">
          {usage.reported_requests > 0 && (
            <EstimateValue estimate={usage.estimate} />
          )}
        </span>
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
          部分估算只计可确定的用量和费率，不含未采集的用量、工具费、平台费用及折扣。
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
