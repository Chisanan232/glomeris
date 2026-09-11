# Pressure Model

The `monitor` module implements pure disk-pressure state logic plus the
polling loop that drives it. All platform-specific I/O (real `statvfs`,
real notifications) lives under `platform::macos`, which the monitor's
traits abstract away.

## `PressureState`

Defined in `src/monitor/pressure.rs`, in ascending urgency order:

```rust
pub enum PressureState {
    Healthy,
    Warn,
    Pressured,
    Critical,
    Emergency,
}
```

Rendered (`Display`) as `HEALTHY`, `WARN`, `PRESSURED`, `CRITICAL`,
`EMERGENCY`.

## Thresholds

`ThresholdConfig::default()` (`src/monitor/config.rs`) defines both a
percentage-used bound and an absolute free-bytes bound per state:

| State | used % ≥ | free bytes ≤ |
|---|---|---|
| Warn | 75.0 | 50 GiB |
| Pressured | 85.0 | 20 GiB |
| Critical | 92.0 | 10 GiB |
| Emergency | 97.0 | 3 GiB |

A threshold is "crossed" when *either* bound is crossed (percentage OR
absolute bytes) — not both. Classification checks most-urgent-first
(Emergency → Critical → Pressured → Warn → Healthy), so one reading that
crosses several boundaries lands on the single most urgent one. These are
library defaults; `ThresholdConfig` is passed as a parameter to `monitor::run`,
so a caller could supply different values, but nothing in this codebase does
so today.

## Debounce / hysteresis

`PressureStateMachine::observe()` (`src/monitor/state_machine.rs`) requires
`confirm_after` consecutive matching observations of a *candidate* state
before confirming a transition. The production default is `confirm_after = 2`
(two consecutive one-minute polls, at the default 60-second poll interval).

- If the newly observed state equals the currently-confirmed state, any
  in-flight candidate is discarded.
- If a *different* candidate than the pending one appears, its counter
  resets to 1 — candidates are not averaged.
- Once the counter reaches `confirm_after`, the transition confirms and a
  `Transition { from, to }` is emitted exactly once.
- The same logic and the same `confirm_after` value apply in both directions
  — there is no asymmetry between escalating and recovering (no "escalate
  fast, recover slow" rule exists in this code).

This exists specifically to prevent a single noisy sample from producing a
notification on every poll.

## The polling loop

`monitor::run()` (`src/monitor/poller.rs`) starts the state machine assuming
`Healthy`, then loops: `stat` the watched path, classify the reading, feed it
to the state machine, and — only on a confirmed transition — call the
notifier and the persistence backend. Sleep happens *between* iterations.
`max_iterations: Option<u64>` bounds the loop for tests; production passes
`None` to run indefinitely.

**Error handling is deliberately non-fatal at every layer:**

- A failed `FsStat::stat()` call is reported to the `on_iteration` callback
  as an outcome-less iteration and does not stop the loop.
- A notifier failure or a persistence-write failure is captured into the
  poll outcome's `notify_error`/`persist_error` fields — the loop keeps
  running either way. This is proven by a test that drives 20 successful
  loop iterations against an always-failing persistence backend.

## Persistence

`FilePersistence` (`src/monitor/persistence.rs`) appends one tab-separated
line per confirmed transition (`unix_time_secs`, `from`, `to`,
`used_percent`, `free_bytes`) to a plain text file — not a database. Its own
module doc states it is "explicitly failure-tolerant: any error here must
never stop the monitor loop from continuing to observe and notify." No
feature in this crate depends on persistence succeeding. Structured (SQLite)
persistence is deferred to a later ticket.

## What the monitor does *not* do

The monitor is a cheap, O(1) capacity check (`statvfs`) — it never walks the
directory tree. Discovering *what* is reclaimable is the scanner's and
detectors' job (see [Architecture](architecture.md)), a deliberately separate
and more expensive path.
