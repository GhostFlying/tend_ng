use crate::codebase::{Query, read_json, request_until};
use protocol::{
    CodebaseCommentDiff, CodebaseCommentPosition, CodebaseCommentSource, CodebaseReviewComment,
    PullRef,
};
use serde_json::Value;
use std::{
    collections::HashSet,
    ffi::OsStr,
    thread,
    time::{Duration, Instant},
};

pub(crate) type Snapshot = Query<Vec<CodebaseReviewComment>>;
// Interactive clients and Runner sync use a five-second request timeout.
pub(crate) fn link_deadline() -> Instant {
    Instant::now() + Duration::from_secs(4)
}

const INCOMPLETE: &str = "incomplete_comment_snapshot";

pub(crate) fn snapshot(program: &OsStr, pull: &PullRef, deadline: Instant) -> Snapshot {
    let number = pull.number.to_string();
    let mr = request_until(
        program,
        &["mr", "get", &number, "-R", &pull.repository],
        deadline,
    )?;
    let repo = id(&mr["repository"]["id"]).ok_or("invalid_response")?;
    let mr_id = id(&mr["merge_request"]["Id"]).ok_or("invalid_response")?;
    if mr["repository"]["path"] != pull.repository
        || mr["merge_request"]["Number"] != pull.number
        || id(&mr["merge_request"]["TargetRepoId"]).is_some_and(|target| target != repo)
    {
        return Err("invalid_response");
    }

    // These are the only raw actions the observer can issue. Both are read-only,
    // even though Codebase dispatches its action-based API by HTTP POST.
    let (threads, events) = thread::scope(|scope| {
        let threads = scope.spawn(|| {
            read_json(
                program,
                &[
                    "api",
                    "ListThreads",
                    "-i",
                    "-f",
                    "CommentableType=merge_request",
                    "-f",
                    &format!("CommentableId={mr_id}"),
                    "-f",
                    &format!("RepoId={repo}"),
                ],
                deadline,
            )
        });
        let events = read_json(
            program,
            &[
                "api",
                "ListTimelineEvents",
                "-i",
                "-f",
                "TrackableType=merge_request",
                "-f",
                &format!("TrackableId={mr_id}"),
                "-f",
                &format!("RepoId={repo}"),
                "-F",
                "For1024=true",
            ],
            deadline,
        );
        (threads.join().expect("comment CLI reader"), events)
    });
    parse(pull, &repo, &mr_id, &threads?, &events?)
}

fn id(value: &Value) -> Option<String> {
    // Keep provider string IDs verbatim (including leading zeros). Never
    // coerce large numeric IDs through floating point.
    value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
}

fn optional_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

fn complete(value: &Value) -> bool {
    value.as_object().is_some_and(|fields| {
        fields.iter().all(|(key, marker)| {
            match key.to_ascii_lowercase().replace('_', "").as_str() {
                "hasmore" | "hasnextpage" | "truncated" | "istruncated" | "partial"
                | "ispartial" => marker.is_null() || marker.as_bool() == Some(false),
                "complete" | "iscomplete" => marker.as_bool() == Some(true),
                "nextpagetoken" | "nextpagecursor" | "nextcursor" | "nextpage" | "nexttoken" => {
                    marker.is_null() || marker.as_str() == Some("")
                }
                "pagination" | "pageinfo" => marker.is_null(),
                _ => true,
            }
        })
    })
}

fn list<'a>(envelope: &'a Value, key: &str) -> Query<&'a [Value]> {
    if !envelope["Status"]
        .as_u64()
        .is_some_and(|status| (200..300).contains(&status))
    {
        return Err("codebase_query_failed");
    }
    if envelope["Status"] == 206
        || envelope["Headers"].as_object().is_some_and(|headers| {
            headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("content-range"))
        })
    {
        return Err(INCOMPLETE);
    }
    let body = &envelope["Body"];
    if !body["ResponseMetadata"]["Error"].is_null() {
        return Err("codebase_query_failed");
    }
    let result = &body["Result"];
    let items = result[key].as_array().ok_or(INCOMPLETE)?;
    if !complete(body)
        || !complete(result)
        || !count_matches(body, items.len())
        || !count_matches(result, items.len())
    {
        return Err(INCOMPLETE);
    }
    Ok(items)
}

fn count_matches(container: &Value, expected: usize) -> bool {
    container
        .get("TotalCount")
        .is_none_or(|total| total.as_u64() == Some(expected as u64))
}

fn scope_matches(entry: &Value, repo: &str, mr_id: &str) -> bool {
    [
        ("RepoId", repo),
        ("CommentableId", mr_id),
        ("TrackableId", mr_id),
    ]
    .into_iter()
    .all(|(key, expected)| entry[key].is_null() || id(&entry[key]).as_deref() == Some(expected))
        && ["CommentableType", "TrackableType"]
            .into_iter()
            .all(|key| entry[key].is_null() || entry[key] == "merge_request")
}

fn parse(pull: &PullRef, repo: &str, mr_id: &str, threads: &Value, events: &Value) -> Snapshot {
    let mut output = Vec::new();
    let mut thread_ids = HashSet::new();
    let mut comment_ids = HashSet::new();
    for thread in list(threads, "Threads")? {
        let thread_id = id(&thread["Id"]).ok_or(INCOMPLETE)?;
        let rows = thread["Comments"].as_array().ok_or(INCOMPLETE)?;
        if !thread_ids.insert(thread_id.clone())
            || !scope_matches(thread, repo, mr_id)
            || !complete(thread)
            || !count_matches(thread, rows.len())
        {
            return Err(INCOMPLETE);
        }
        for row in rows {
            let comment_id = id(&row["Id"]).ok_or(INCOMPLETE)?;
            if !comment_ids.insert(comment_id.clone())
                || !scope_matches(row, repo, mr_id)
                || row.get("Content").is_none()
                || !row["ThreadId"].is_null() && id(&row["ThreadId"]).as_ref() != Some(&thread_id)
            {
                return Err(INCOMPLETE);
            }
            let mut comment = evidence(
                pull,
                CodebaseCommentSource::ThreadComment,
                comment_id,
                row,
                &row["CreatedBy"],
                &row["Content"],
            )?;
            comment.thread_id = Some(thread_id.clone());
            location(&mut comment, row, thread);
            output.push(comment);
        }
    }

    let mut event_ids = HashSet::new();
    for event in list(events, "Events")? {
        let kind = event["Type"]
            .as_str()
            .ok_or(INCOMPLETE)?
            .to_ascii_lowercase();
        if !matches!(
            kind.as_str(),
            "review" | "review_submitted" | "submitted_review" | "merge_request_reviewed"
        ) {
            continue;
        }
        let nested = &event["Detail"]["MergeRequestReviewedEvent"];
        let (body, author) =
            if let Some(review) = nested.get("Review").filter(|review| review.is_object()) {
                let author = review
                    .get("CreatedBy")
                    .filter(|value| !value.is_null())
                    .or_else(|| nested.get("Actor").filter(|value| !value.is_null()))
                    .unwrap_or(&event["Author"]);
                (&review["Content"], author)
            } else {
                // Explicit review text is fine. Do not fall back to a timeline
                // actor's first thread comment (already represented by ListThreads).
                (&event["Content"], &event["Author"])
            };
        if !scope_matches(event, repo, mr_id) {
            return Err(INCOMPLETE);
        }
        // Approval and other review events without explicitly authored text
        // are not comments. Don't interpret their status or other timeline text.
        match body {
            Value::Null => continue,
            Value::String(text) if text.trim().is_empty() => continue,
            Value::String(_) => {}
            _ => return Err(INCOMPLETE),
        }
        let event_id = id(&event["Id"]).ok_or(INCOMPLETE)?;
        if !event_ids.insert(event_id.clone()) {
            return Err(INCOMPLETE);
        }
        let mut comment = evidence(
            pull,
            CodebaseCommentSource::ReviewNote,
            event_id,
            event,
            author,
            body,
        )?;
        let review = &nested["Review"];
        comment.thread_id = id(&review["ThreadId"]).or_else(|| id(&event["ThreadId"]));
        location(&mut comment, review, event);
        output.push(comment);
    }
    Ok(output)
}

fn evidence(
    pull: &PullRef,
    source: CodebaseCommentSource,
    id: String,
    row: &Value,
    author: &Value,
    body: &Value,
) -> Query<CodebaseReviewComment> {
    let body = match body {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        _ => return Err(INCOMPLETE),
    };
    Ok(CodebaseReviewComment {
        id,
        source,
        repository: pull.repository.clone(),
        number: pull.number,
        url: pull.url(),
        thread_id: None,
        username: optional_text(&author["Username"]),
        display_name: optional_text(&author["DisplayName"]),
        created_at: optional_text(&row["CreatedAt"]).ok_or(INCOMPLETE)?,
        body,
        positions: Vec::new(),
        diff: None,
        outdating_diff: None,
    })
}

fn location(comment: &mut CodebaseReviewComment, row: &Value, parent: &Value) {
    if let Some(pos) = position(&row["Position"]).or_else(|| position(&parent["Position"])) {
        comment.positions.push(pos);
    } else if let Some(positions) = row["Positions"]
        .as_array()
        .or_else(|| parent["Positions"].as_array())
    {
        comment
            .positions
            .extend(positions.iter().filter_map(position));
    }
    comment.diff = diff(row, parent, "TruncatedDiff");
    comment.outdating_diff = diff(row, parent, "OutdatingTruncatedDiff");
}

fn diff(row: &Value, parent: &Value, field: &str) -> Option<CodebaseCommentDiff> {
    [row, parent].into_iter().find_map(|entry| {
        let diff = &entry[field];
        let binary = diff["Binary"].as_bool();
        let truncated = diff["Truncated"].as_bool();
        let raw_patch = diff["RawPatch"].as_str().map(str::to_owned);
        (binary.is_some() || truncated.is_some() || raw_patch.is_some()).then_some(
            CodebaseCommentDiff {
                binary,
                truncated,
                raw_patch,
            },
        )
    })
}

fn position(value: &Value) -> Option<CodebaseCommentPosition> {
    let location = CodebaseCommentPosition {
        kind: optional_text(&value["Type"]),
        side: optional_text(&value["Side"]),
        path: optional_text(&value["Path"]),
        old_commit: optional_text(&value["OldCommitId"]),
        new_commit: optional_text(&value["NewCommitId"]),
        start_line: value["StartLine"].as_i64(),
        start_column: value["StartColumn"].as_i64(),
        end_line: value["EndLine"].as_i64(),
        end_column: value["EndColumn"].as_i64(),
    };
    (location.kind.is_some()
        || location.side.is_some()
        || location.path.is_some()
        || location.old_commit.is_some()
        || location.new_commit.is_some()
        || location.start_line.is_some()
        || location.start_column.is_some()
        || location.end_line.is_some()
        || location.end_column.is_some())
    .then_some(location)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pull() -> PullRef {
        PullRef::parse("https://code.byted.org/example/guide/merge_requests/73").unwrap()
    }

    fn envelope(key: &str, rows: Value) -> Value {
        json!({"Status":200,"Body":{"ResponseMetadata": {}, "Result": {key: rows}}})
    }

    #[test]
    fn captures_inline_reply_and_explicit_review_notes_without_guessing() -> anyhow::Result<()> {
        let created = "2026-11-26T09:00:00Z";
        let threads = envelope(
            "Threads",
            json!([{
                "Id": "thread", "RepoId": "201", "CommentableId":"80173", "Status":"resolved",
                "Positions":[{"Path":"src/example.rs","StartLine":9,"Side":"new"}],
                "TruncatedDiff":{"Truncated":true,"RawPatch":"+ example"},
            "OutdatingTruncatedDiff":{"RawPatch":"- example"},
                "Comments":[
                    {"Id":"00123","ThreadId":"thread","CreatedAt":created,"Content":"first",
                     "CreatedBy":{"Username":"alice","DisplayName":"Alice"},
                     "Position":{"Path":"src/example.rs","StartLine":10}},
                    {"Id":"00124","CreatedAt":created,"Content":"reply"},
                ],
                "DraftComments":[{"Id":"draft","Content":"still private"}],
            }]),
        );
        let notes = envelope(
            "Events",
            json!([
                {"Id":"00123","Type":"merge_request_reviewed","CreatedAt":created,"Detail":{
                    "MergeRequestReviewedEvent":{
                        "Actor":{"Username":"actor"},
                        "Review":{"CreatedBy":{"Username":"reviewer","DisplayName":"Reviewer"},"Content":"Please review the fixtures","Status":"approved"}
                    }
                }},
                {"Id":"approve","Type":"merge_request_reviewed","CreatedAt":created,
                 "Detail":{"MergeRequestReviewedEvent":{"Review":{"Status":"approved"}}}},
                {"Id":"merged","Type":"merge_request_merged","Content":"status event, not a comment"},
                {"Id":"status","Type":"merge_request_updated","Content":"status change"},
                {"Id":"timeline-copy","Type":"review_submitted","CreatedAt":created,
                 "Detail":{"Another":{"Comments":[{"Content":"not an independent review note"}]}}},
            ]),
        );
        let comments = parse(&pull(), "201", "80173", &threads, &notes).unwrap();
        assert_eq!(comments.len(), 3, "no pure approval or timeline copies");
        let [line, reply, note] = comments
            .iter()
            .filter(|row| !row.body.is_empty())
            .collect::<Vec<_>>()[..]
        else {
            panic!("unexpected comment identities {comments:#?}");
        };
        assert_eq!(line.id, "00123");
        assert_eq!(reply.id, "00124");
        assert_eq!(line.created_at, reply.created_at);
        assert_eq!(line.positions[0].start_line, Some(10));
        assert_eq!(reply.positions[0].start_line, Some(9));
        assert_eq!(reply.thread_id.as_deref(), Some("thread"));
        assert_eq!(
            reply.diff.as_ref().unwrap().raw_patch.as_deref(),
            Some("+ example")
        );
        assert_eq!(
            reply.outdating_diff.as_ref().unwrap().raw_patch.as_deref(),
            Some("- example")
        );
        assert_eq!(line.username.as_deref(), Some("alice"));
        assert_eq!(line.display_name.as_deref(), Some("Alice"));
        assert_eq!(note.username.as_deref(), Some("reviewer"));
        assert_eq!(note.id, line.id);
        assert_ne!(note.source, line.source);
        let serialized = serde_json::to_value(note)?;
        assert!(serialized.get("thread_id").is_none());
        assert!(serialized.get("positions").is_none());
        assert_eq!(serialized["url"], pull().url());
        Ok(())
    }

    #[test]
    fn incomplete_is_never_equivalent_to_an_explicit_empty_snapshot() {
        let threads = envelope("Threads", json!([]));
        let notes = envelope("Events", json!([]));
        assert!(
            parse(&pull(), "201", "80173", &threads, &notes)
                .unwrap()
                .is_empty()
        );
        for broken in [
            json!({"Status":206,"Headers":{"content-range":"items 0-0/2"},"Body":{"Result":{"Threads":[]}}}),
            json!({"Status":200,"Headers":{"Content-Range":"items 0-0/2"},"Body":{"Result":{"Threads":[]}}}),
            json!({"Status":200,"Body":{"TotalCount":1,"Result":{"Threads":[]}}}),
            envelope("Other", json!([])),
            envelope("Threads", Value::Null),
            envelope("Threads", json!([{"Id":"omitted-nested"}])),
            json!({"Status":200,"Body":{"Result":{"Threads":[],"HasMore":true}}}),
            json!({"Status":200,"Body":{"Result":{"Threads":[],"TotalCount":1}}}),
            json!({"Status":200,"Body":{"Result":{"Threads":[],"NextPageToken":"continue"}}}),
            json!({"Status":200,"Body":{"Result":{"Threads":[]},"Pagination":{}}}),
            json!({"Status":200,"Body":{"ResponseMetadata":{"Error":{"Code":"permission"}},"Result":{"Threads":[]}}}),
            json!({"Status":403,"Body":{"Result":{"Threads":[]}}}),
            json!({"Status":200,"Body":{"Result":{"Threads":[{"Id":"x","Comments":[
                {"Id":"duplicated","Content":"a","CreatedAt":"2026-11-26T09:00:00Z"},
                {"Id":"duplicated","Content":"b","CreatedAt":"2026-11-26T09:00:00Z"}
            ]}]}}}),
        ] {
            assert!(
                parse(&pull(), "201", "80173", &broken, &notes).is_err(),
                "{broken}"
            );
        }
        assert_eq!(
            parse(
                &pull(),
                "201",
                "80173",
                &threads,
                &envelope("Other", json!([]))
            ),
            Err(INCOMPLETE)
        );
    }
}
