import type { DiscussionProgressItem } from "./api";

export function DiscussionProgress({
  items,
  pending,
}: {
  items: DiscussionProgressItem[];
  pending: boolean;
}) {
  const latest = items.at(-1);
  if (!latest) return null;
  return pending ? (
    <div className="discussion-progress discussion-progress-live" role="status">
      <span className="discussion-progress-label">进行中</span>
      <span className="discussion-progress-latest">{latest.text}</span>
    </div>
  ) : (
    <details className="discussion-progress discussion-progress-history">
      <summary>查看回合进展 · {items.length} 条</summary>
      <ol>
        {items.map(({ attempt_id, item_id, text }) => (
          <li key={`${attempt_id}:${item_id}`}>{text}</li>
        ))}
      </ol>
    </details>
  );
}
