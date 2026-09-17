# hydrozoa-archiver

Copies a Hydrozoa head's store out from under a running node, so the node can trim it.

```bash
hydrozoa-archiver -c /etc/hydrozoa-archiver/config.json
hydrozoa-archiver -c config.json --once      # one full pass, then exit
hydrozoa-archiver -c config.json --dry-run   # what a pass would copy; copies nothing
```

## How it reaches the store

It opens the node's own RocksDB directory as a **secondary** — a read-only handle that catches up
from the primary's WAL and manifest on demand. There is no socket, no IPC and no coordination: the
node keeps writing throughout and is never told this reader exists.

```
  /opt/hydrozoa/store/
        ▲              ▲
        │ read+write   │ read only
   hydrozoa node   archiver
```

Because a secondary writes nothing into the primary directory, read-only is enforceable rather than
promised: **run the archiver as its own user with `r-x` on the store directory.** Then the kernel
holds the guarantee, not this process.

The files are local, so the archiver runs on the box that hosts the node.

## What it writes

A second RocksDB in the node's own layout — same families, same key encodings, values stored
exactly as the node stored them including the 12-byte arrival-stamp prefix. Restoring is copying it
back, not converting it.

Everything is archived: the six journal types plus the eighteen fixed families (snapshots,
confirmations, reverse indices). That is the difference between an archive a node can be restored
from and one that is only an audit trail.

Two cadences, because only the journals can be resumed:

| | families | resumes from | cadence |
|---|---|---|---|
| tail | the 6 journal types | the archive's own tip | `tail_interval_secs` (5) |
| full | tail, plus the 18 fixed families | nothing — full scan | `full_interval_secs` (300) |

`DepositMap` and `Treasury` are single blobs rewritten in place; the confirmation families key by
block or stack; the reverse indices key by request id. None has an index to resume from, so each
costs a whole-family scan whether anything changed or not — hence the slower cadence.

**One catch-up per pass, not per family.** A secondary's view only moves when told to, so
everything in a pass comes from a single consistent moment and the snapshots line up with the
journals by construction.

The archive needs no state file: the resume point is the archive's own tip, so a crashed pass costs
a little re-copying and nothing else.

## Identity

The archive carries the node store's `StoreIdentity` stamp — head params hash, head id, head
address, own peer id — written on first use and checked on every open afterwards. Archives are
per-node and all look alike on disk; without the stamp nothing would stop peer 1's journals being
appended into peer 0's archive, producing a file that looks whole and means something else.

This requires store schema **version 3 or later**, where the stamp was introduced. Against an
earlier store the archiver refuses rather than copying bytes it cannot attribute.

## Gaps

If the node has deleted journal entries this archive never copied, the pass reports a **gap** and
keeps going. Nothing can recover that range — it is gone from both sides — so it is logged at
error rather than passed over. A fresh archive against an already-trimmed store is *late*, not
holed: it starts at the node's retention floor and reports nothing.

## Telling the node

After a pass is durably fsynced, the archiver posts the highest archived index per family to the
node's admin API:

```
POST {node_url}/api/admin/archive/watermark
```

Per-family rather than one number, because the journals advance at independent rates and a scalar
could only carry the minimum — holding retention back to the slowest lane. The node answers with
the floor it actually adopted after taking the minimum with what consensus still needs.

Every archive write is fsynced before this is sent. Hydrozoa itself does not fsync, so the
archiver's own fsync is the only durability barrier in the chain, and a watermark sent ahead of it
would authorise deleting data that exists nowhere.

**With no `node_url` configured the archiver archives and reports nothing**, which leaves the node
retaining everything — the safe direction. Failing to report is likewise never fatal: the archive
is already durable, and the only consequence is that the node keeps more.

## Config

Built-in defaults, then `-c file.json` (repeatable), then `HYDROZOA_ARCHIVER_*` env vars.

```json
{
  "store_path": "/opt/hydrozoa/store",
  "secondary_path": "/var/lib/hydrozoa-archiver/secondary",
  "archive_path": "/var/lib/hydrozoa-archiver/archive",
  "tail_interval_secs": 5,
  "full_interval_secs": 300,
  "node_url": "http://127.0.0.1:8080",
  "admin_username": "admin"
}
```

Supply `admin_password` through `HYDROZOA_ARCHIVER_ADMIN_PASSWORD` rather than the file.

⛔ `secondary_path` must not be shared with another reader. RocksDB does not lock it and creates no
LOCK file there, so two readers sharing one corrupt each other's view **with no error at all**. The
process id is appended for exactly that reason.
