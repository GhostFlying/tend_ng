# Inline proposal approval

## Goal and limits

Present an explicitly versioned proposal, its report, and the original Issue discussion in one document. The Issue and original Chat show one recorded user decision. Existing questions remain ordinary questions. This design does not act on any existing live question, copy production content, or authorize deployment.

## Phases

1. Freeze the synthetic [design reference](mockups/inline-proposal-approval.html).
2. Add explicit proposal authoring for a report and a concrete approval action. Store immutable candidate versions under the original Attention. Do not turn older questions or other reports into proposals by inference.
3. Validate the exact version at the shared CLI and Web API. Commit the decision, original Task input and Issue timeline placement atomically. The handoff uses the existing AttentionAnswered input and its actual native Run evidence. No Router turn or Chat message is created by approval.
4. Add the Issue approval strip, report and collapsed history, original Chat action or status, and paginated non-message approval events. Use the existing Markdown renderer and artifact boundary. Keep independent or ambiguous decisions available in their original Chat.
5. Verify an immutable candidate before pushing main. Download and check the official main build for that SHA. No deployment.

## Verification

- Verify revision identity, stale writes, duplicate clicks, lost responses, exact report and Task ownership, independent decisions, and absence of extra Chat or Router inputs. Exercise delayed Run takeover, including a Run that has not started its native turn.
- Verify ordinary questions and previous client and Runner wire shapes. Keep proposal metadata out of delivered Runner inputs.
- Use isolated synthetic data for desktop and mobile Chromium inspection. Exercise the normal Issue entry, approval, original Chat, property disclosure, Markdown scrolling, and history. Preserve original screenshots separately.
