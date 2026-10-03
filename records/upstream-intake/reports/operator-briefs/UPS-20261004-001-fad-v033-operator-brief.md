# FAD v0.3.3 operator brief

## Review Metadata

- Review id: UPS-20261004-001
- Opened: `2026-10-04 01-10-00 KST`
- Recorded by agent: codex
- Review date: 2026-10-04
- Upstream window: `7857b2d...f3c00b7`
- Baseline reviewed against: fork `16e73e2`
- Overall recommendation: adapt and publish

## This Period At A Glance

Upstream now supplies Muse and native VS Code Copilot, so the fork retires its
duplicate parsers. Miniharness, bundled rusqlite, and small attribution fixes
remain. FAD passes 1,244 tests; Heatmap passes 807. Legacy Copilot databases need
the `copilot-vscdb` feature; older string answers remain supported.

## Decisions Requiring Operator Input

None for this authorized sync.

## Watchlist

- New FAD OpenCode schema support does not automatically change Heatmap's direct
  database adapter.
- Keep Miniharness and SQLite backend differences explicit for later retirement.
- API assessment coverage is partial; test success does not prove every upstream
  behavior or independently verified landing.

## Decisions Made Autonomously

Use upstream parsers and keep only consumer-required adaptations. Preserve both
pinned histories with a generated merge commit; imported upstream messages are
an explicit migration exception. Publish the fork, then pin and deploy Heatmap.
