# Which drive a chain is on

Each chain has its own file (docs/SPLITDB.md), so each chain can sit on its own drive: Bitcoin on
a big hard drive, the home chain on the fast one.

## In the app

**Chains & Coins → Install → Drive.** Pick this arcade's drive (the default) or another mounted
disk. The list shows free space on each one. A disk the arcade can't write to yet shows the
command that fixes that, for example:

```sh
sudo mkdir -p /mnt/big/arcade-chains && sudo chown $USER: /mnt/big/arcade-chains
```

**An installed chain:** its panel shows **Stored on …**, with the folder, size and free space, plus
**Move to [drive] → Move**. The move runs in the background:

1. The chain's sync and address build stop. The chain stays installed.
2. Its file is copied to the new drive. This is mdbx's compact copy, a consistent snapshot, so
   free pages are left behind. The panel shows GB copied.
3. The copy is opened and its tip checked against the chain's. Then the chain switches to it, the
   old copy is deleted, and the sync goes on from where it was.

If anything fails, the chain stays where it was and syncs on. The panel and Events say why. The
home chain stays on this arcade's drive for now.

## From a terminal

```sh
arcade chain drives                         # drives, free space, writable or not
arcade chain add btc --mode full --drive /mnt/big
arcade chain move btc /mnt/big              # or: arcade chain move btc home
arcade chain status btc                     # place and move progress (also over /rpc)
```

## How it is kept

- On another drive, the chain's file is `<mount>/arcade-chains/<key>/chain.mdbx`. The arcade's
  `chains/<key>.place` holds that folder's path, one line. On this arcade's drive it is
  `chains/<key>/chain.mdbx` with no place file, as before.
- The place file is written before the old copy is set aside, so a stop at any point leaves a
  whole copy where the place file points. A leftover `chains/.<key>.old` or `.<key>.moving` is
  cleaned up by the next move.
- If the drive isn't mounted when the arcade starts, the log says so and the chain doesn't open.
  Its writes are refused rather than starting the chain over on the empty mount point. It syncs
  again once the drive is back and the arcade restarts.
- Removing the chain deletes its folder on that drive and its place file.
- Rows written to the old file by something other than the chain's sync during the copy are
  left behind. Today that is only mempool rows, which come back from the network.
- Linux only: drives come from `/proc/self/mounts` and free space from `statvfs`. The Windows
  branch needs its own drive list.
