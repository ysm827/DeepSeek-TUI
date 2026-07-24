# Post-0.9.1: thin TUI over core + stream consolidation

**Status:** seams landed in v0.9.1; full split deferred.

## What shipped in 0.9.1 (seams only)

New visual / capability systems stay in focused modules (do not grow the
monoliths without necessity):

| System | Module |
|--------|--------|
| Ambient ocean life | `tui/ambient_life.rs` |
| Hot tail | `tui/hot_tail.rs` |
| Hover aura | `tui/hover_hit.rs` + `tui/hover_layer.rs` |
| Git status cache | `tui/git_status.rs` |
| Worktree manager UI | `tui/worktree_manager.rs` |
| Phase rail | `tui/phase_strip.rs` |
| Stream entry seam | `client/stream_entry.rs` |

Business logic must not land in `ui.rs` / `app.rs` / `widgets/mod.rs` unless it
is pure view wiring.

## StreamFn consolidation (landed post-0.9.1)

`client/stream_entry.rs` is the shared open-path seam, and all three
streaming adapters open through it:

- HTTP policy (`DualWithH1Fallback` / `Http1Only`, env pin via
  `CODEWHALE_FORCE_HTTP1`)
- dual/H1-twin client selection (`client_for_policy`)
- bounded response-header wait (`stream_open_timeout`, env override
  `CODEWHALE_STREAM_OPEN_TIMEOUT_SECS`)
- one shared open function (`open_sse_response`): a classified H2 header
  stall on the dual client retries exactly once on the HTTP/1.1 twin;
  an H1-pinned request never retries; nothing retries once response
  headers (and therefore any stream body) exist
- H1 retry classification (`should_retry_with_h1`)
- idle-timeout message format (`idle_timeout_message`, with
  bytes/age/last-chunk diagnostics)

Wire-protocol request construction and stream decoding remain at the
adapter edge (`chat.rs`, `anthropic.rs`, `responses.rs`): each adapter
builds its own endpoint URL, headers, auth, and body inside the attempt
closure it hands to `open_sse_response`. The pre-existing Responses
provider retry loop (rate limit / transient upstream, `send_with_retry`)
stays inside each open attempt, before any stream body exists.

Remaining follow-up: collapsing further toward a piagent-style single
StreamFn (shared decode loop) is still deferred.

## Thin TUI over core (north star)

`ui.rs` / `app.rs` / `widgets/mod.rs` remain large. Post-0.9.1 priority:

1. Extract tool / git / github / session / workflow / MCP routing out of the TUI
   crate into a core/data layer (kimi-code `agent-core` / piagent package shape).
2. Keep the TUI a projection of state + input routing.
3. Prefer new modules over adding to the three monoliths.

## Optional deferred

- Full live global model subscriptions (refresh on every open + `r` / Ctrl+R is
  the practical path; continuous live feed if unstable stays deferred).
- YOLO mode is gone from product UI; `mode_yolo` remains only as legacy theme
  palette data.
