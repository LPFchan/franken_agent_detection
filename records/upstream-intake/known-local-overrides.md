# Known Local Overrides

Use this register to record intentional downstream divergences so they do not have to be rediscovered from scratch every review.

Only record stable, intentional divergences here.
Do not use this file for temporary experiments or unreviewed preferences.

## Entry Template

- Area:
- Local surface:
- Upstream surface:
- Why the fork diverged:
- Collision rule to apply during intake:
- Revisit trigger:
- Related decision record:

## Current Entries

- Area: Conversation connector coverage
- Local surface: `src/connectors/muse.rs`, Muse detection, fixtures, and exports
- Upstream surface: No Muse connector at base `7857b2d` (upstream issue #15 documents the schema)
- Why the fork diverged: Downstream heatmap needs a public native connector for Muse Code logs.
- Collision rule to apply during intake: Preserve the local implementation while comparing upstream changes; merge or retire it only after equivalent upstream support is reviewed.
- Revisit trigger: Upstream adds compatible Muse support, or Muse's storage/event schema changes.
- Related decision record: none yet.
