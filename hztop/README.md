# hztop

A btop-style terminal dashboard for operating a Hydrozoa head. Two screens under a persistent
chrome (alerts, head, confirmation):

| Screen | Key / subcommand | Contents |
|---|---|---|
| consensus | `c` / `1` | heads side-by-side with tip skew, throughput, consensus & mempool, request sources, slowest blocks |
| alerts | `a` / `2` | timestamped raised/cleared history with durations |

```
cargo build --release -p hztop
./target/release/hztop -c my-config.json   # last-viewed screen
./target/release/hztop a                   # open on alerts
```

Keys: `q` quit · `tab` switch head peer · `←`/`→` or `c a` / `1`-`2` switch screen.

The last-viewed screen and the alert history persist in `~/.local/state/hztop/state.json`
(`$XDG_STATE_HOME` respected).

## Data sources

Everything comes from a head peer's own HTTP API. Each source is optional and degrades to
"unavailable" on its own:

| Source | What it provides |
|---|---|
| `/head/stats` | request rates, blocks, stacks, timings, mempool, headroom |
| `/ready` | `initializing` \| `active` \| `finalized` \| `handed-off-to-rule-based` |
| `/version` | the node's build version |
| `/head/blocks/{tip}` | current version (major.minor), confirmation status, fallback deadline |

`hztop` reaches a head over HTTP only, so it runs from anywhere that can reach one — there is no
local-disk requirement.

## Config

Merged like the node itself: built-in defaults, then `-c file.json` (repeatable), then `HZTOP_*`
env vars. With no `-c`, the first existing of `$HZTOP_CONFIG`, `~/.config/hztop/config.json`,
`/etc/hztop/config.json` is loaded — so a deployed box with a config in place just runs `hztop`.
Defaults point at localhost:8080.

```json
{
  "heads": [
    { "name": "head-0", "url": "http://127.0.0.1:8080" },
    { "name": "head-1", "url": "http://127.0.0.1:8081" }
  ]
}
```

## JSON output

`hztop --json` prints one snapshot of everything the dashboard knows and exits. It is also the
default whenever stdout is not a terminal, so `hztop | jq ...`, a script, or an agent gets data
instead of escape codes a pipe cannot render.

`--json-timeout-ms` (default 10000) bounds the wait for the heads' first poll. The snapshot is
printed either way; `complete: false` says a head never answered, and that head's own `reachable`
field says which.

## Alerts

The strip at the top turns yellow/red when: a head is unreachable or not `active`; there has been
no hard confirmation for `hard_confirm_warn_secs`/`hard_confirm_crit_secs` (1800/3600); the
fallback window opens within `fallback_warn_secs`/`fallback_crit_secs` (1800/600); sequencer
headroom is nearly exhausted; or backpressure rejections occurred in the last minute. All
thresholds are config keys.
