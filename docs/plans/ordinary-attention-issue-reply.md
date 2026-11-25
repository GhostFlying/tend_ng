# Answer an ordinary Attention from its Issue

## Scope and stop point

Provide a small free-text reply action for each eligible, published ordinary Attention in the Issue detail. Use the original Attention ID and preserve the submitted text. The existing Chat reply, structured proposal, ordinary Issue message and board flows retain their meaning. No production access, state changes, or deployment.

**Confirmed boundary:** Extend the existing original-Task exception for proposal approvals to inputs created by this new direct reply operation only. Reject an already terminated original Task. If termination happens after acceptance but before native continuation, keep the recorded answer on the original Task without reporting that it continued. The ordinary Chat path, Runner protocol, and assignment reconciliation are unchanged.

## Source evidence

- `crates/tend-server/src/store/attention.rs:105–134` writes the genuine `AttentionAnswered` input to the supplied Task and updates the original Attention in one transaction. The existing Router response at `:307–354` checks the original Chat message and keeps its identity. The direct Issue operation can use the same writer with no synthetic user message.
- `crates/tend-server/src/store/coordination.rs:535–545` may create a follow-up Task after a terminal observation if there is pending work. `:594–641` also creates a follow-up when independent input arrives at a terminal Task and moves its pending inputs. Both paths exclude review inputs and records in `proposal_decisions`, not an ordinary Issue reply. Merely requiring a live Task at acceptance does not cover the race between commit and Run takeover.
- `crates/tend-server/src/store/proposals.rs:273–288` saves the existing original-Task exception atomically with the wake. Commit `c8e883c198019a4861ce8b8849e278586a65bd8f` introduced the two exclusion predicates for that reason. Reusing a proposal decision row would misrepresent a free-text reply as a version approval.
- `crates/tend-server/src/store/board.rs:334–348` and the Chat Attention projection derive their state from the same Attention row. The board badge disappears only when no other open published question remains.

## Implementation

1. Add an authenticated Issue-scoped write operation, shared by Web and CLI, that identifies one Attention and carries the user's text and stable operation ID. Expose the original Chat question and exact Attention in the Issue overview. Resolve scope from the same Issue-tree and original Chat relationships as the existing decision list. Reject unpublished, structured, out-of-scope, already answered, or stale original Task targets. Use the existing live-assignment and pending-stop checks as a starting point. Reject blank or oversized input without rewriting valid text.
2. Use the existing `answer_task` writer. Persist a separate direct-reply receipt with the operation, scope, and exact input identity in the same short transaction. Replay an identical committed request even after state changes and reject reuse with different parameters. Never publish a Chat or Issue message or enqueue a Router input on behalf of the user.
3. Use that receipt for the same two follow-up exclusions already used by proposal approvals, limited to direct Issue replies. If the original Task becomes terminal after the answer is stored and before native takeover, keep the answer visible as recorded without claiming the Agent resumed. Keep legacy Chat replies, structured approvals, and unrelated successor inputs unchanged. Do not start a substitute Task for the answer.
4. Render one explicitly scoped form per eligible question and render the recorded answer as plain text from the original fact. Reuse the original Chat status and board projections. Make a lost-response retry reuse the original operation and text. Keep the ordinary discussion composer and structured approval UI distinct.

## Acceptance

- Use an isolated real Server, Runner, CLI, and synthetic App Server. Verify one durable input with exact text, one genuine continuation on the same Task, no new Task or Router input, original Chat status, and the natural board badge behavior. Verify that a normal Issue message has no Attention effect and that multiple published questions cannot cross-answer.
- Exercise lost-response replay, operation or target mismatch, request for a structured or unpublished question, and stale, closed, or stopped targets. Include terminal observation after commit and unrelated follow-up after termination. Verify old Chat replies, existing structured approval, and Runner protocol-window compatibility.
- Use Playwright with a built Web app and synthetic fixture. On desktop and narrow viewports, open and answer a specific question, verify resulting rendering and original Chat state, inspect overflow and console, and save actual screenshots outside the repo.
- Review the immutable candidate, run local checks, and verify the full remote main SHA and its CI if implementation is authorized. Do not open a PR or deploy.

## Independent review follow-up

- A visible historical ordinary Chat question refreshes its existing per-Attention read while awaiting a user answer or native continuation. It stops when delivered or outside the viewport. Latest-page refresh and historical structured proposals keep their existing paths. A two-tab fixture keeps the original historical question mounted while an Issue tab submits the direct answer.
- Keep every pending decision in the Issue overview. Include only the five newest direct replies, ordered by receipt identity, and expose older replies through an authenticated keyset page loaded on demand, shared with `tend issue attention-replies <issue> [before]`. Select the bounded receipt slice before pulling full text. Older pages are not reloaded by the overview poll. Verify a large synthetic reply history, unchanged pending visibility, stable cursor paging, and the rendered load-more action.
- Keep every settled reply already shown while browsing older pages, even as later overview polls move the recent slice or the older cursor is exhausted. If newer and previous slices do not overlap, keep the visible rows and offer an explicit restart from the newest slice to recover any unobserved gap. After a keyboard user loads the final page, move focus from the retiring control to the first revealed reply, without stealing focus if they moved elsewhere.
