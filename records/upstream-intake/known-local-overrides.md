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

- Area: Miniharness connector
- Local surface: `miniharness.rs`, registry, token extraction, and exports
- Upstream surface: No Miniharness connector at `f3c00b7`.
- Why the fork diverged: Heatmap consumes summon JSONL without embedding its accounting policy in FAD.
- Collision rule to apply during intake: Retire the local parser after equivalent upstream support passes consumer fixtures.
- Revisit trigger: Upstream Miniharness support.
- Related decision record: UPS-20261004-001.

- Area: SQLite dependency implementation
- Local surface: Cargo optional features, `sqlite_sync.rs`, SQLite connector row bindings
- Upstream surface: fsqlite and asupersync.
- Why the fork diverged: Retain bundled rusqlite without the restricted dependency family.
- Collision rule to apply during intake: Port new readers while preserving query, snapshot, and schema behavior.
- Revisit trigger: Upstream adopts an acceptable compatible backend.
- Related decision record: UPS-20261004-001.

- Area: Attribution and consumer compatibility
- Local surface: Claude Desktop selected-folder handling; Copilot remote URI and legacy string response handling; Muse root export and legacy alias
- Upstream surface: Upstream Muse and Copilot parsers; local filesystem URI handling.
- Why the fork diverged: Preserve Heatmap source and workspace attribution and existing transcript generations.
- Collision rule to apply during intake: Prefer upstream implementations once matching regression fixtures pass.
- Revisit trigger: Upstream fixes each remaining gap.
- Related decision record: UPS-20261004-001.

- Area: Explicit OpenCode scan boundaries
- Local surface: `allow_local_default_dbs`, fixture scan contexts
- Upstream surface: Local explicit roots also search the machine's default databases.
- Why the fork diverged: Explicit roots must not introduce conversations from unrelated real stores.
- Collision rule to apply during intake: Preserve default local discovery while restricting explicit roots; scan and discovery agree.
- Revisit trigger: Upstream adopts equivalent explicit-root boundaries.
- Related decision record: UPS-20261004-001.
