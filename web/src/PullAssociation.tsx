import type { PullLink } from "./api";

const pullLabels: Record<string, string> = {
  open: "开启",
  merged: "已合并",
  closed: "已关闭",
};

const checkLabels: Record<string, string> = {
  success: "CI 通过",
  pending: "CI 进行中",
  failure: "CI 失败",
};

export function pullSectionTitle(pulls: PullLink[]) {
  const codebase = pulls.some((pull) => pull.provider === "codebase");
  const github = pulls.some((pull) => pull.provider === "github");
  return codebase ? (github ? "关联 PR / MR" : "关联 MR") : "关联 PR";
}

export function PullAssociation({
  pull,
  showStatus = true,
}: {
  pull: PullLink;
  showStatus?: boolean;
}) {
  const marker = pull.provider === "codebase" ? "!" : "#";
  const facts = pull.facts;
  const state =
    facts?.draft && facts.state === "open"
      ? "草稿"
      : facts && pullLabels[facts.state];
  return (
    <>
      <a className="pull-link" href={pull.url} target="_blank" rel="noreferrer">
        {pull.repository} {marker}
        {pull.number}
      </a>
      {showStatus && (
        <div className="pull-status">
          {state && (
            <span>
              {pull.observation_error ? "上次：" : ""}
              {state}
            </span>
          )}
          {facts?.ci && checkLabels[facts.ci] ? (
            <span data-check={facts.ci}>
              {pull.observation_error ? "上次：" : ""}
              {checkLabels[facts.ci]}
            </span>
          ) : (
            facts && <span>CI 未确认</span>
          )}
          {pull.observation_error && (
            <span data-observation-error>暂不可观测</span>
          )}
          {!facts && !pull.observation_error && <span>待观测</span>}
        </div>
      )}
    </>
  );
}
