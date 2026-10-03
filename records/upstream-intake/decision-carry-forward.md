# Decision Carry-Forward

Use this register to preserve intake outcomes that should automatically inform later reviews.

This exists to stop the same `accept`, `adapt`, `decline`, or `defer` question from being re-litigated every cycle without new evidence.

## Entry Template

- Candidate area:
- First decision date:
- Most recent confirmation date:
- Current standing decision: `accept` | `adapt` | `decline` | `defer`
- Carry-forward rationale:
- What new evidence would justify reopening this:
- Related report, ADR, or note:

## Current Entries

- Candidate area: FAD upstream synchronization
- First decision date: 2026-10-04
- Most recent confirmation date: 2026-10-04
- Current standing decision: `adapt`
- Carry-forward rationale: Prefer upstream Muse and Copilot implementations; preserve Miniharness, bundled rusqlite, attribution, explicit scan boundaries, and legacy transcript compatibility until upstream equivalents pass consumer fixtures.
- What new evidence would justify reopening this: Upstream covers a remaining override, an acceptable SQLite dependency backend appears, or verified provider schemas require a change.
- Related report, ADR, or note: UPS-20261004-001.
