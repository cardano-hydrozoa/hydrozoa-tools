# hydrozoa-tools

Operator tools for a [Hydrozoa](https://github.com/cardano-hydrozoa/hydrozoa) head. Three crates;
none of them writes to a node.

| crate | what it is |
|---|---|
| [`hztop/`](hztop/README.md) | a btop-style terminal dashboard for a running head, over its HTTP API |
| [`hydrozoa-store/`](hydrozoa-store/) | reads a head's RocksDB store as a secondary, without stopping the node |
| [`hydrozoa-archiver/`](hydrozoa-archiver/README.md) | copies that store into an archive, so the node can trim it |

## Build

Everything runs inside the flake devshell:

```bash
nix develop --command just test     # what CI gates on
nix develop --command just top      # hztop against a head on localhost:8080
```

Outside the devshell `librocksdb-sys` has no `ROCKSDB_LIB_DIR` and recompiles its bundled C++
source — minutes and ~950 MB per build configuration. The flake pins the toolchain; there is no
`rust-toolchain.toml`, deliberately, so a rustup toolchain and the devshell's cannot silently
disagree.

## `hydrozoa-store`

A hydrozoa head keeps its consensus history in RocksDB. This crate opens that store as a
**secondary**: a read-only handle that catches up from the primary's WAL and manifest on demand.
The node keeps writing throughout and never learns the reader exists.

```rust
let store = HeadStore::open(&primary, &unique_secondary(&scratch))?;
store.catch_up()?;

for cf in store.journals() {
    let floor = store.floor(cf)?;          // oldest entry still retained
    let tip = store.tip(cf)?;              // newest
    for entry in store.scan(cf, from)? {
        let entry = entry?;                // index, arrival stamp, payload bytes
    }
}
```

Journal payloads come back encoded. An archiver copies them verbatim and never has to understand
them, so the crate carries no codec for hydrozoa's wire types — only the Request lane's protobuf
`RequestRecord`, which it needs to read a request's kind.

The storage contract is transcribed from hydrozoa's `multisig/persistence/` (`Cf`, `JournalKey`,
`JournalValue`, `ArrivalStamp`, `StoreVersion`, `StoreIdentity`) and `proto/request_record.proto`.
Hydrozoa is the only writer. The golden fixtures under `hydrozoa-store/tests/golden/` are byte-for-byte
copies of hydrozoa's own, which is what the proto asks a reader in another repo to do — if the
encoding changes, the test fails rather than the format drifting silently.

`SUPPORTED_STORE_VERSION` is **3**. A store written in another version is reported, not guessed at.

## `hztop`

See [`hztop/README.md`](hztop/README.md). Two screens — consensus and alerts — fed entirely by a head's
own HTTP API. `hztop --json` (and any run whose stdout is not a terminal) prints the same state as
one JSON document, so a script or an agent reads what the terminal draws.

## `hydrozoa-archiver`

See [`hydrozoa-archiver/README.md`](hydrozoa-archiver/README.md). Copies the node's store into a
second RocksDB in the same layout, then tells the node how far it durably reached so the node may
trim. Reading needs no cooperation from the node at all — that one report is the only coupling
between the two processes.
