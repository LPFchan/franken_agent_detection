# franken-agent-detection Plans

## Planning Rules

- Only accepted future direction belongs here.
- Plans should be specific enough to guide execution later.
- Product or architecture rationale should link to `DEC-*` records when relevant.
- When a plan becomes current truth, reflect it into `records/SPEC.md` or `records/STATUS.md` and update this file.

## Approved Directions

### Upstream synchronization

- Outcome: Review upstream changes on a recurring cadence while preserving the
  Muse local override until upstream support exists or the fork retires it.
- Why this is accepted: The fork should remain easy to rebase without losing
  the downstream capability.
- Expected value: Smaller, explicit sync conflicts and durable compatibility notes.
- Preconditions: Keep `records/upstream-intake/` active and record each accepted
  review in its paired report surfaces.
- Earliest likely start: After the fork is published.
- Related ids: upstream base `7857b2d`.

### Muse schema watch

- Outcome: Extend Muse parsing only when new schemas are verified by fixtures
  and can map faithfully into normalized contracts.
- Why this is accepted: Muse telemetry and usage records are not automatically
  conversation messages or per-message token attribution.
- Expected value: Correct downstream indexing without fabricated semantics.
- Preconditions: New issue or fixture evidence.
- Earliest likely start: When a schema change is observed.
- Related ids: upstream issue #15.

## Sequencing

### Near Term

- Initiative: Commit and publish the completed fork implementation, then pin it in downstream heatmap and complete the private Muse parser cutover.
  - Why now: The connector is implemented and verified, and downstream can consume it once the fork is available remotely.
  - Dependencies: Parent commit/push and a downstream dependency update.
  - Related ids: none.

### Mid Term

- Initiative: Run the first upstream-intake review against the fork's local Muse divergence.
  - Why later: The fork publication establishes the review baseline.
  - Dependencies: A landed implementation and an upstream comparison window.
  - Related ids: upstream issue #15.

### Deferred But Accepted

- Initiative: Add non-Linux Muse storage roots.
  - Why deferred: The available evidence verifies Linux XDG paths only.
  - Revisit trigger: Reproducible provider documentation or synthetic fixtures for another platform.
