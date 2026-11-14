# bitcoinconsensus 0.106.0+26.0, patched for Arcade V1

A copy of the crates.io crate (MIT; Bitcoin Core 26.0's libbitcoinconsensus) with one addition,
marked `ARCADE PATCH`:

- `depend/bitcoin/src/script/interpreter.cpp`: `arcade_set_sighash_prefix`, a per-thread 32-byte
  prefix that `SignatureHash` writes ahead of the message (legacy and witness v0), as Luckycoin's
  interpreter writes its `CHAIN_ID` from block 276,500 ("revival" signatures).
- `src/lib.rs`: `verify_with_flags_and_sighash_prefix`, which sets the prefix for one call.

With no prefix set every hash is Bitcoin's own. Wired in by `[patch.crates-io]` in the workspace
Cargo.toml.
