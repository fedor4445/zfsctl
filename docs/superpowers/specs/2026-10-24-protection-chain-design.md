# Protection follows the chain: design

Status: built (2026-10-24). Extends `2026-10-23-unprotected-datasets-design.md`.

## Intent

zfsctl's mission is to protect data, and the first rule of a backup is to not lose the original. A copy is only a copy
of something: protection is a property of the *data*, not of one dataset. A replica on cold storage holds the same data
as its source, so the two are judged together, and the cloud (or whatever is offsite) has the final word on whether
that data is safe.

## What changed

- A dataset that a **current** replication to another host writes into is a *replica*. The original and all its replicas
  (through any number of hops) are **one body of data**, judged once; the verdict is shown on every dataset in it.
- The copies of a body: each replica, plus every other current copy job (restic, cloud mirror, replication to a file)
  of any dataset in the body. A place that several jobs write to counts once. The original is never a copy of itself.
- The level (`copies`, `online`) is asked of the body, using the original's tier.
- A replica needs no snapshot job while the replication feeding it is current. If that replication is not current the
  link is cut: the replica stands alone (so it is listed), and the original loses the copy.
- Replications in a circle have no original: each dataset is judged alone and its outgoing replications are its copies.
- Before this change a replica was exempt. Now it warns whenever the body is short of the level, so a cold-storage
  host that holds the only copy of something no cloud has shows the warning too.

## Decisions made for the operator to check

1. `a -> b` and `a -> cloud`: `b`'s replica is **clear**, because the body has a cloud copy; a separate cloud job on `b` is
   not demanded. (The alternative, demanding one, would nag about data the cloud already holds.)
2. At Basic (one copy on another machine) a replication alone clears both sides. The cloud is mandatory only at Careful
   or Custom with `online >= 1`.
3. A restic repository or mirror job on a *replica* counts for the original, and the other way round.

## Later (not built)

- **Marker in the data** (a ZFS user property, stamped by the cloud job, carried by `zfs send -p`): cold storage that
  says on its own where its other copies are, even without the hub. A claim, not proof: only fresh stamps would count.
- **Graph measures** across all hosts: freshness along a path (a cloud copy reached through `a -> b -> cloud` is no
  newer than the two lags added), failure domains (count independent places, not names: two VMs on one box are not two
  copies; the host `tag` could mark a domain), single points of failure (an articulation point on every path to the
  cloud), and hop count.
- A replica whose feeding replication is broken still reads `no snapshot job`; it could name the replication instead.
