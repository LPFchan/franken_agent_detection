# franken-agent-detection Plans

## Approved Directions

### Upstream synchronization

- Outcome: Continue recurring upstream review and retire local implementations
  when upstream covers their behavior.
- Preconditions: Paired upstream-intake reports and consumer compatibility checks.
- Related ids: UPS-20261004-001.

### Provider schema watch

- Outcome: Extend provider parsing only from reproducible schemas and fixtures.
- Preconditions: Preserve raw evidence, discovery parity, and consumer accounting
  boundaries; verify non-Linux Muse paths before adding them.
- Related ids: upstream issue #15.

## Sequencing

### Near Term

- Initiative: Update Heatmap's pinned dependency to the published v0.3.3 integration,
  retaining `copilot-vscdb` for legacy SQLite transcripts.

### Deferred But Accepted

- Initiative: Offer the Miniharness connector upstream when its public contract
  is ready for upstream review.
- Revisit trigger: A verified shared consumer requirement.
