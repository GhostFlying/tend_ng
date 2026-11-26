import { useEffect, useState } from "react";
import { findReferences } from "./discussionApi";
import type { IssueChoice } from "./discussionApi";

export function discussionError(value: unknown) {
  return value instanceof Error ? value.message : "暂时无法完成请求";
}

export function useReferenceSearch(query: string, target: boolean) {
  const [found, setFound] = useState<{
    query: string;
    target: boolean;
    results: { repos: string[]; issues: IssueChoice[] };
  } | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    const timer = window.setTimeout(
      async () => {
        try {
          const results = await findReferences(
            query,
            target,
            controller.signal,
          );
          if (controller.signal.aborted) return;
          setFound({ query, target, results });
          setError("");
        } catch (error) {
          if (!controller.signal.aborted) setError(discussionError(error));
        }
      },
      query ? 150 : 0,
    );
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [query, target]);
  return {
    result:
      found?.query === query && found.target === target ? found.results : null,
    error,
  };
}
