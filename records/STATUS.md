# franken-agent-detection Status

## Snapshot

- Last updated: 2026-08-19
- Overall posture: `active`
- Current focus: Miniharness connector publication and downstream heatmap cutover
- Highest-priority blocker: none
- Next operator decision needed: upstream sync timing after the fork lands
- Related decisions: none recorded yet

## Current State Summary

The fork is based on upstream commit `7857b2d740dcdcfb4f834f9e394a873fe1796a4d` and
retains both `origin` (the LPFchan fork) and `upstream` remotes. The repo-template
scaffold, including the optional upstream-intake module and local hooks, is
installed. Native Muse and Miniharness support are implemented behind the
existing `connectors` feature. The Copilot connector now reads every native VS
Code transcript generation: `interactive.sessions` in `state.vscdb`, flat chat
session JSON, and append-log JSONL. It covers workspace and empty-window stores
while rejecting non-Copilot sessions from the shared VS Code chat store.
Miniharness normalizes summon JSONL while leaving accounting and visibility
policy to consumers. SQLite-backed connectors use bundled `rusqlite`; the
restricted `fsqlite` and `asupersync` dependency family is no longer present.
Focused connector tests, feature checks, formatting, and lint checks pass.

## Active Phases Or Tracks

### Template adoption

- Goal: Keep fork policy, truth surfaces, hooks, and upstream review workflow durable.
- Status: `done`
- Why this matters now: The fork needs a stable boundary for local divergence and upstream sync.
- Current work: None.
- Exit criteria: Scaffold present, hooks installed, fork facts seeded.
- Dependencies: repo-template `1.1.5`.
- Risks: Template upgrades can conflict with project-local truth docs.
- Related ids: none.

### Muse connector

- Goal: Detect, discover, and normalize Muse root and nested subagent logs.
- Status: `done`
- Why this matters now: Downstream heatmap can replace its private native parser after the fork publishes.
- Current work: Parent commit/push and downstream dependency pin/cutover.
- Exit criteria: Feature tests, formatting, lint checks, and discovery parity pass.
- Dependencies: Muse Code 0.1.0 schema described by upstream issue #15.
- Risks: The observed schema is a small Linux-only corpus and may evolve.
- Related ids: upstream issue #15.

### Miniharness connector

- Goal: Detect, discover, and normalize Miniharness summon JSONL without carrying downstream accounting policy.
- Status: `done`
- Why this matters now: Downstream heatmap can replace its private transcript parser with the shared connector.
- Current work: Publication and downstream dependency cutover.
- Exit criteria: Discovery parity, envelope variants, malformed input, filename fallback, multiple usage messages, and token evidence are covered by tests.
- Dependencies: Miniharness's stable JSONL session contract and default root.
- Risks: Consumers must choose their own session-level aggregation and visibility policy.
- Related ids: none.

### VS Code Copilot storage compatibility

- Goal: Normalize Copilot transcripts from every native VS Code persistence generation.
- Status: `done`
- Why this matters now: Recovered Application Support trees contain workspace-scoped history that the old globalStorage-only adapter missed.
- Current work: Publish the fork commit and let downstream Heatmap refresh from the persistent VS Code tree.
- Exit criteria: SQLite, flat JSON, append-log JSONL, empty-window storage, Copilot filtering, deduplication, and discovery parity pass tests.
- Dependencies: VS Code's `interactive.sessions` and `ChatSessionStore` persistence contracts.
- Risks: Provider-neutral VS Code chat files without Copilot ownership evidence are intentionally skipped.
- Related ids: VS Code commits `a4ee2666` and `5438d07d`.

## Recent Changes To Project Reality

- Date: 2026-08-19
  - Change: Replaced the optional `fsqlite` and `asupersync` stack with bundled `rusqlite` across all SQLite-backed connectors.
  - Why it matters: Public consumers can use the connectors without inheriting the restricted Franken dependency rider, while retaining embedded SQLite builds.
  - Related ids: none.

- Date: 2026-08-15
  - Change: Added native VS Code workspace, empty-window, and historical SQLite support to the Copilot connector.
  - Why it matters: Persistent Application Support trees can replace lossy one-off Copilot imports without missing older database eras.
  - Related ids: VS Code commits `a4ee2666` and `5438d07d`.

- Date: 2026-08-13
  - Change: Adopted repo-template `1.1.5` and enabled tracked hooks.
  - Why it matters: Fork operations now have canonical truth and provenance surfaces.
  - Related ids: upstream base `7857b2d`.
- Date: 2026-08-13
  - Change: Completed native Muse detection, scanning, normalization, exports, and synthetic tests.
  - Why it matters: Downstream consumers have a public connector capability to pin.
  - Related ids: upstream issue #15.
- Date: 2026-08-13
  - Change: Completed native Miniharness detection, scanning, normalization, per-message usage extraction, exports, and synthetic tests.
  - Why it matters: Consumers can parse summon sessions without importing Heatmap accounting policy.
  - Related ids: none.

## Active Blockers And Risks

- Blocker or risk: Muse event vocabulary and platform paths are only partially verified.
  - Effect: Future Muse releases or non-Linux installations may be missed.
  - Owner: Fork maintainer.
  - Mitigation: Keep detection Linux XDG-only and preserve unknown event evidence.
  - Related ids: upstream issue #15.

## Immediate Next Steps

- Next: Commit and publish the reviewed Miniharness connector.
  - Owner: Parent/orchestrator.
  - Trigger: Parent review completion.
  - Related ids: none.
- Next: Pin the published fork in downstream heatmap and retire its private Miniharness transcript parser.
  - Owner: Parent/orchestrator.
  - Trigger: Fork commit and push available.
  - Related ids: none.
