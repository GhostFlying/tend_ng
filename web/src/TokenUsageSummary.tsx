import type { TokenBreakdown, TokenField } from "./api";

export const format = new Intl.NumberFormat("zh-CN");

export function tokens(field: TokenField) {
  return field.tokens === null ? "—" : format.format(BigInt(field.tokens));
}

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

export function RunCoverage({ usage }: { usage: TokenBreakdown }) {
  if (usage.started_runs === 0) {
    return <p className="detail-muted token-coverage">没有已开始的 Run。</p>;
  }
  return (
    <div className="token-coverage">
      <p>
        <strong>{coverageLabel(usage)}</strong>
        <span>
          已观测 {format.format(usage.observed_runs)} /{" "}
          {format.format(usage.started_runs)} 个已开始 Run
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

export function coverageLabel(usage: TokenBreakdown) {
  if (usage.started_runs === 0) return "没有已开始的 Run";
  if (usage.observed_runs === 0) return "完全未观测";
  if (usage.observed_runs < usage.started_runs) return "部分覆盖";
  return "已观测全部 Run";
}

export function TokenTotal({ usage }: { usage: TokenBreakdown }) {
  return (
    <>
      <div className="token-total">
        <bdi>{tokens(usage.total_tokens)}</bdi>
        <span>Token</span>
      </div>
      <p className="detail-muted token-source">
        App Server 原生上报
        {fieldNote(usage.total_tokens, usage)
          ? ` · ${fieldNote(usage.total_tokens, usage)}`
          : ""}
      </p>
    </>
  );
}
