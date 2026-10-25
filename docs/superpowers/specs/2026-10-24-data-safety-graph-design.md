# The data-safety graph: design

Status: slices 1 (model), 2 (Dashboard pop-up) built and installed 2026-10-24; slice 3 half built (the host `site` key); the minimum-domains requirement and slice 4 not built.

## Intent

zfsctl's mission is to protect data, and the first rule of a backup is to not lose the original. Counting copies says how
many there are, not how safe the data is: two copies on two VMs of one physical machine are not two copies, and a cloud
copy that is fed through a replica is only as fresh and as alive as that replica. The data and its copies form a graph
across every host the hub manages; this note says what zfsctl measures on it and where it shows it.

## The graph

- **Places** hold the data: the original (a dataset on a host), each replica (a dataset a current or stopped replication
  writes into), and each copy made by a restic backup, a cloud mirror or a replication to a file.
- **Edges** are the copy jobs, from the dataset the job reads to the place it writes. An edge knows its job, when the job
  last succeeded and whether it is current (the same rule as the protection check).
- A **body** is an original and everything reachable from it. Datasets with no copy job have no body (the protection check
  already lists them); replications in a circle have no original and make no body (the check judges them alone).

## What is measured (all conservative: when in doubt, report the worse)

1. **Age of the data at a place** (the recovery point). The original is live. A place reached by an edge holds the data as
   old as the *oldest last success on the path*: `a -> b -> cloud` with the first hop last done 10 minutes ago and the
   second 3 hours ago holds data at most 3 hours old. A hop that never succeeded makes the age unknown, never zero.
2. **Hops**: the length of the path from the original.
3. **Current**: every edge on the path is current. A place that is not current is shown, but is no protection.
4. **Failure domains.** Each place sits in a domain: its host, unless the operator says several hosts share a failure
   (one physical machine, one rack, one site); a cloud remote is its own domain; the hub is the domain of a file target.
   The body **tolerates N failures** where N is the number of distinct domains holding a current copy (the original's
   included) minus one: that many domains can be lost at once and the data survives.
5. **Dependents / single points**: the places that stop being fed if a given place fails (everything below it in the
   feeding tree). The body names a replica that is the only way into the cloud.

## Slices

1. The pure model (`daemon/safety.rs`) with tests: places, edges, bodies, age, hops, current, tolerance, dependents.
2. A screen: the bodies as trees with ages and the numbers above, the weakest first (a section or a pop-up; the operator
   decides).
3. Failure domains from `config.toml` (`site` on a host) and a requirement in `[protection]` (a minimum of independent
   domains), so the check can ask for it, per group like the other keys.
4. The daily report: the weakest bodies in a few lines.

## Decisions made for the operator to check

- The age rule is the **oldest hop**, not a sum of schedule lags: it needs only what the history already records.
- A place not reached by a *current* path does not count towards the tolerance.
- Slice 1 has no configuration: domains come from a function the caller gives (the host name for now).
