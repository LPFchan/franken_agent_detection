# franken-agent-detection Spec

- Project: LPFchan fork of `franken-agent-detection`
- Canonical repo: https://github.com/LPFchan/franken_agent_detection
- Upstream: https://github.com/Dicklesworthstone/franken_agent_detection
- Project id: `franken-agent-detection`
- Operator: LPFchan (GitHub: `LPFchan`)
- Last updated: 2026-08-13

## Project thesis

Provide deterministic, local filesystem detection and normalized conversation
connectors for coding agents. The fork stays source-compatible with upstream
where practical while carrying explicitly documented downstream connectors
that upstream has not accepted yet.

## Core capabilities

- Detect known agent installations from supported default roots and explicit
  per-connector overrides.
- Discover source files and normalize conversations into the crate's shared
  message and invocation contracts.
- Support Muse Code's Linux XDG session logs, including nested subagents.

## Invariants

- Connector discovery and scanning cover the same source files.
- Provider sequence fields are authoritative when a provider documents them.
- Unsupported or ambiguous provider data remains raw evidence rather than being
  assigned invented semantics.
- No private transcripts, credentials, or generated build output belong in the
  repository.

## Non-goals

- Claiming unverified Muse storage paths on macOS or Windows.
- Per-message Muse token attribution when the source schema lacks correlation
  identifiers; token events remain available as message evidence where needed.
