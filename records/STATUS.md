# franken-agent-detection Status

## Snapshot

- Last updated: 2026-10-04
- Overall posture: `active`
- Current focus: downstream Heatmap cutover after the published v0.3.3 integration
- Highest-priority blocker: none
- Next operator decision needed: none
- Related ids: UPS-20261004-001

## Current State Summary

The fork integrates upstream `f3c00b742756692567539037c48745dfc3e87f5a`
(v0.3.3), preserving upstream ancestry and the prior fork head
`16e73e25ac92628d7dc3dc5cf21438225b210cfc`. Upstream's Muse and native
VS Code Copilot implementations replace the duplicate local parsers.
Miniharness, the root Muse export, Claude Desktop selected-folder attribution,
remote VS Code workspace paths, and bundled rusqlite remain local adaptations.
Legacy string-form Copilot responses are retained. Historical VS Code database
support now requires the upstream `copilot-vscdb` feature.

All 1,244 active all-feature/all-target FAD tests pass; one remains ignored.
Heatmap's integration suite passes 807 tests; four remain ignored. SQLite-backed
connectors use bundled rusqlite without fsqlite or asupersync. OpenCode explicit
roots bound scans and discovery; temporary-store tests do not read real history.

## Active Blockers And Risks

- Upstream connector coverage does not automatically migrate Heatmap's own
  direct OpenCode database adapter to the newer OpenCode schema.
- Muse storage evidence remains principally Linux XDG. Accounting and visibility
  policy belong to consumers.
- The installed nightly reports an unknown upstream Clippy lint allowance;
  actionable all-target Clippy diagnostics are resolved.

## Immediate Next Steps

- Complete the Heatmap cutover to published integration `c416a920b445f9e9e468eeefd25c48314afb8461`.
- Keep future local changes registered in the override list and test consumers
  whenever upstream changes normalization or optional feature gates.
