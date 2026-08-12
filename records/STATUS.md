# franken-agent-detection Status

## Snapshot

- Last updated: 2026-08-13
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
existing `connectors` feature. Miniharness normalizes summon JSONL while
leaving accounting and visibility policy to consumers. Focused and full
connector tests pass.

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

## Recent Changes To Project Reality

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
