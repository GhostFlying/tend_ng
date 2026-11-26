import { issueLink, issueTitle } from "./api";
import type { IssueMessage } from "./api";

export function DispatchAuthor({
  provenance,
}: {
  provenance: IssueMessage["dispatch_provenance"];
}) {
  if (provenance?.origin === "main_chat") {
    return provenance.agent_name ? (
      <>
        <span className="author">{provenance.agent_name}</span>
        <span className="message-state">来自主会话</span>
      </>
    ) : (
      <span className="author">来自主会话</span>
    );
  }
  if (provenance?.origin === "other_chat") {
    return <span className="author">来自其他会话</span>;
  }
  return (
    <span className="author">{provenance?.agent_name ?? "Issue 委派"}</span>
  );
}

export function IssueTakeoverEvent({
  message,
  descendant,
}: {
  message: IssueMessage;
  descendant: boolean;
}) {
  return (
    <li className="takeover-row message-heading" id={`message-${message.id}`}>
      <span>
        <strong>{message.author}</strong> 已接手
      </span>
      {descendant && (
        <a
          className="issue-link"
          href={issueLink(message.issue_id, message.short_number)}
        >
          {issueTitle(message.issue_title, message.short_number)}
        </a>
      )}
    </li>
  );
}
