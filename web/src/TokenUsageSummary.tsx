import type { TokenBreakdown, TokenEstimate, TokenField } from "./api";
import { compactTokens, displayUsd, format, tokens } from "./tokenDisplay";

export function fieldNote(
  field: TokenField,
  usage: Pick<TokenBreakdown, "reported_requests">,
) {
  if (field.tokens === null) {
    return usage.reported_requests > 0 ? "字段缺失" : "未知";
  }
  if (field.reported_requests < usage.reported_requests) {
    return `${format.format(field.reported_requests)} / ${format.format(usage.reported_requests)} 条请求有此字段`;
  }
  return undefined;
}

export function RunCoverage({
  usage,
  issueRunsOnly = false,
}: {
  usage: TokenBreakdown;
  issueRunsOnly?: boolean;
}) {
  const unit = issueRunsOnly ? "Issue Run" : "Run";
  if (usage.started_runs === 0) {
    return <p className="detail-muted token-coverage">没有已开始的 {unit}。</p>;
  }
  return (
    <div className="token-coverage">
      <p>
        <strong>{coverageLabel(usage, unit)}</strong>
        <span>
          已观测 {format.format(usage.observed_runs)} /{" "}
          {format.format(usage.started_runs)} 个已开始 {unit}
        </span>
      </p>
      {usage.unobserved_before_collection > 0 && (
        <p className="detail-muted">
          采集启用前 · {format.format(usage.unobserved_before_collection)} 个
        </p>
      )}
      {usage.unobserved_no_usage > 0 && (
        <p className="detail-muted">
          已开始但无可用用量事件 · {format.format(usage.unobserved_no_usage)} 个
        </p>
      )}
      {usage.observed_runs > 0 && (
        <p className="detail-muted">
          至少收到一条有效请求即为已观测，无法确认尾部完整。
        </p>
      )}
    </div>
  );
}

export function coverageLabel(usage: TokenBreakdown, unit = "Run") {
  if (usage.started_runs === 0) return `没有已开始的 ${unit}`;
  if (usage.observed_runs === 0) return "完全未观测";
  if (usage.observed_runs < usage.started_runs) return "部分覆盖";
  return `已观测全部 ${unit}`;
}

const rawFields = [
  ["total_tokens", "总 Token"],
  ["input_tokens", "输入"],
  ["cached_input_tokens", "缓存读取"],
  ["cache_write_input_tokens", "缓存写入"],
  ["output_tokens", "输出"],
  ["reasoning_output_tokens", "推理输出"],
] as const;

export function RawTokenFields({
  usage,
}: {
  usage: Pick<
    TokenBreakdown,
    (typeof rawFields)[number][0] | "reported_requests"
  >;
}) {
  return (
    <dl className="model-raw">
      {rawFields.map(([field, label]) => {
        const note = fieldNote(usage[field], usage);
        return (
          <div key={field}>
            <dt>{label}</dt>
            <dd>
              <bdi>{tokens(usage[field])}</bdi>
            </dd>
            {note && <dd className="detail-muted">{note}</dd>}
          </div>
        );
      })}
    </dl>
  );
}

export function Amount({ usd }: { usd: string }) {
  return <bdi className="model-cost">{displayUsd(usd)}</bdi>;
}

export function RequestCount({
  usage,
  identified = false,
}: {
  usage: { estimate?: TokenEstimate; reported_requests: number };
  identified?: boolean;
}) {
  const unpriced = usage.estimate?.unpriced_requests ?? 0;
  return (
    <span className="usage-request-counts detail-muted">
      <span>
        {format.format(usage.reported_requests)} 条{identified && "已识别"}请求
      </span>
      {unpriced > 0 && (
        <span className="model-unpriced-count">
          {format.format(unpriced)} 条请求未计价
        </span>
      )}
    </span>
  );
}

function UsageAmount({
  usage,
}: {
  usage: { estimate?: TokenEstimate; reported_requests: number };
}) {
  if (usage.reported_requests === 0)
    return <span className="detail-muted">—</span>;
  const usd = usage.estimate?.usd;
  return usd == null ? (
    <span className="detail-muted usage-missing-amount">无可计金额</span>
  ) : (
    <Amount usd={usd} />
  );
}

export function UsageMetrics({
  usage,
  compact = false,
  showEstimate = true,
}: {
  usage: Pick<
    TokenBreakdown,
    | "input_tokens"
    | "cached_input_tokens"
    | "output_tokens"
    | "estimate"
    | "reported_requests"
  >;
  compact?: boolean;
  showEstimate?: boolean;
}) {
  const value = compact ? compactTokens : tokens;
  const cached = usage.cached_input_tokens;
  return (
    <span className="model-line-fields" data-estimate={showEstimate}>
      <span className="model-line-field">
        <span className="detail-muted">输入</span>
        <bdi>{value(usage.input_tokens)}</bdi>
        <span className="usage-cache detail-muted">
          <span>其中缓存读取</span>
          {cached.tokens === null ? (
            <span>{usage.reported_requests === 0 ? "—" : "未记录"}</span>
          ) : (
            <>
              <bdi>{value(cached)}</bdi>
              {cached.reported_requests < usage.reported_requests && (
                <span>（已记录部分）</span>
              )}
            </>
          )}
        </span>
      </span>
      <span className="model-line-field">
        <span className="detail-muted">输出</span>
        <bdi>{value(usage.output_tokens)}</bdi>
      </span>
      {showEstimate && (
        <span className="model-line-field usage-line-cost">
          <span className="detail-muted">估算费用</span>
          <UsageAmount usage={usage} />
        </span>
      )}
    </span>
  );
}

export function UsageHeadline({ usage }: { usage: TokenBreakdown }) {
  return (
    <>
      <div className="usage-headline">
        <div className="token-total">
          <span>总 Token</span>
          <bdi>{compactTokens(usage.total_tokens)}</bdi>
        </div>
        <div className="usage-headline-amount">
          <span className="detail-muted">估算费用</span>
          <UsageAmount usage={usage} />
        </div>
      </div>
      <RequestCount usage={usage} identified />
    </>
  );
}

export function UsageMethod({
  usage,
  global = false,
}: {
  usage: TokenBreakdown;
  global?: boolean;
}) {
  return (
    <details className="usage-method">
      <summary>统计口径</summary>
      <div className="usage-method-body">
        <p className="detail-muted">
          按 App Server 原生请求汇总
          {global ? "，Issue 只计直属一次，其他调用来源分别列出" : ""}。
          缓存读取已含在输入中，推理已含在输出中，不重复相加。缺报字段不补零。
        </p>
        <p className="detail-muted">
          费用按{" "}
          <a
            href="https://openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties"
            target="_blank"
            rel="noreferrer"
          >
            OpenRouter 公示标准价
          </a>{" "}
          估算，不是账单，不含工具与平台费。只有完全没有可用金额的已识别请求才记为未计价。
          分项缺口与普通输入价基准见模型详情。
        </p>
        {global && (
          <p className="detail-muted">
            Run 覆盖仅指
            Issue。其他来源按各自回合计，无法验证的用量不估算，飞书研判不为计量保存会话原文。
          </p>
        )}
        <RunCoverage usage={usage} issueRunsOnly={global} />
        <RawTokenFields usage={usage} />
      </div>
    </details>
  );
}
