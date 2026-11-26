# Shared chat Markdown

## Goal and boundaries

Render the original message text as Markdown in the shared timeline row used by main Chat, Discussions, and Issue Chat. The Markdown renderer used by reports remains the only parser and URL policy. Apply it to both author roles and to the original-question disclosure when a structured proposal replaces the usual body. Leave reply previews, composer and send behavior, message identity and provenance, proposal cards, and stored/live text unchanged. Do not deploy.

## Implementation

- Use the shared Markdown renderer synchronously in the row so layout effects measure the rendered message on first paint. Give chat its own presentation mode for soft line breaks and the existing bubble treatment, without changing report rendering.
- Keep the existing link allowlist and browser navigation behavior, and continue discarding raw HTML and remote images. Put the entire user message in its existing bubble, including lists and fenced code. Keep wide code scrollable inside the message lane.

## Verification

- Add renderer regressions for `[#81](URL)`, both list types, inline and fenced/indented code, single newlines, safe and blocked link handling, and unchanged report-mode soft breaks.
- Run the Web tests, typecheck, lint, format check, and build. Check the built Tend Web UI against isolated local fixture data in Chromium at desktop and mobile widths. Exercise both sender roles on all three surfaces, check link interaction, raw text, and overflow, and capture screenshots.
- Land on `main` following the repository convention, then verify the exact candidate's Main CI result. No live messages or deployment are in scope.
