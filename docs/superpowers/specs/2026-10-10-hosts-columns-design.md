# Hosts columns, host type and tag, host filter — Design

Date: 2026-10-10

## Goal

Make the Hosts section usable with many hosts (a homelab of a dozen, a site of fifty): see at a glance what each host is, how full its pools are, when it last scrubbed and whether its jobs fail, and narrow the list to the hosts you care about. Name, Address and State stay always visible.

## Scope

- New table columns, all from data the 30-second probe and the job history already hold.
- A host **Type** (`metal`, `vm`, `jail`), detected once per host.
- An optional per-host **Tag** from `config.toml` (and the Add Host form).
- A `/` **filter** on the Hosts table.

Non-goals (YAGNI): sorting or grouping, CPU/memory/load columns (sampled only for the selected host; stay in the bottom panel), editing a tag from the TUI after creation (edit `config.toml`; hosts are read at start), multiple tags per host, hypervisor-specific columns, host actions on several hosts at once.

## Columns

`● Name | Address | State | Type | Tag | Used | Pools | Scrub | Jobs | Rtt`

| Column | Content | Source |
|---|---|---|
| Type | `metal`, `vm`, `jail`, or `-` | detection below |
| Tag | the configured tag, or empty | `config.toml` |
| Used | the highest pool capacity on the host (`85%`), yellow from 80, red from 90 (the same thresholds as State), `-` with no probe | probe |
| Pools | number of pools, `-` with no probe | probe |
| Scrub | `running 34%` / `paused` if any pool is in that state (running first); else `never` if any pool never scrubbed; else the oldest last-scrub date (`Oct  8 2026`); `-` with no probe | probe |
| Jobs | `failed/total` of the host's enabled jobs (`1/4`, failed part in the error colour when above 0), `-` when it has none | job list and history |
| Rtt | as today | probe |

**Narrowing.** Name, Address and State are kept as they are today (Address drops only when State's minimum width cannot be met). The others appear only while they fit, and go in this order as the terminal narrows: Rtt, Jobs, Scrub, Pools, Tag, Used, Type. The Tag column is as wide as the longest tag, at most 14 characters; the others have fixed widths (Type 6, Used 5, Pools 5, Scrub 13, Jobs 5, Rtt 6).

## Host type

Detected with one unprivileged command, `sysctl -n kern.vm_guest security.jail.jailed` (`sysctl` is already a registered unprivileged remote command, so the sudoers line does not change). `jailed` of 1 gives `jail`; otherwise `kern.vm_guest` of `none` gives `metal` and any other value (`bhyve`, `kvm`, `xen`, `vmware`, `hv`, ...) gives `vm`. A failed or unparsable answer (non-FreeBSD host, sysctl missing) gives `-`.

It runs once per host, inside that host's first successful probe, and the result is cached for the session (it cannot change while the host is up). A host whose probe has not yet succeeded shows `-`. The hypervisor name (`bhyve`) is kept in the detected value for later use; v1 shows only the word, and the bottom panel is unchanged.

The Type word has its own colour, distinct from the health colours: `metal` in the normal text colour, `vm` in the accent colour, `jail` in the secondary accent. The `●` and State keep the health colours.

## Tag

`HostConfig` gains `tag: Option<String>` (`tag = "backup"` in `config.toml`; omitted when unset, so existing configs load and save unchanged). The Add Host form gains a third, optional field "Tag", which is the last field (Enter there submits, as the second field does today); a blank tag is stored as none. The tag is trimmed and limited to 40 characters.

## Filter

`/` on the Hosts table opens a one-line filter input in the table's bottom border. Typing narrows the list live: a host is shown when the text (case-insensitive) is a substring of its name, address, tag or type word. Enter keeps the filter and returns to navigating; Esc clears it. The table title shows `Hosts (filter: text — 3/12)` while a filter is set.

`hosts` and `selected_host` stay global indices (every section and the daemon address hosts by them), so the filter only chooses which rows are drawn: Up/Down move among visible hosts, and when the filter hides the selected host the selection moves to the first visible one. The selection therefore always names a visible host, so another section can never act on a host the user cannot see. A filter that matches nothing cannot be committed with Enter (the input stays open with a "no host matches" line; Esc clears it). Adding a host clears the filter.

## Wiring

- `HostProbe::Reached` gains `kind: HostKind` (`Metal`, `Vm(String)`, `Jail`, `Unknown`), filled by `probe_host`; the cached value lives in `HostProbeState` so a later probe that does not repeat the detection keeps it. `probe_host` runs the detection only when asked (a flag from the caller that knows whether a type is cached).
- Pure functions, unit-tested: `host_kind_from_sysctl(output)`, `scan_column(pools)`, `used_column(pools)`, `visible_hosts(hosts, addresses, tags, kinds, filter) -> Vec<usize>`, the column-fitting function.
- `draw_hosts` is rewritten around a column list so the fitting rule is one function.

## Testing

- Unit: sysctl parsing (`none`, `bhyve`, `jailed=1`, garbage, missing); scan column (running beats paused beats never beats a date, oldest date); used column thresholds; jobs column counts; filter matching (name, address, tag, type, case, no match); column fitting at 80, 100, 120, 170 columns (the required three always present, the drop order).
- Config: a config without `tag` loads; with `tag` round-trips; a blank tag is omitted on save.
- UI/state: `/` opens the input, typing narrows, Up/Down skip hidden hosts, the selection moves off a hidden host, Esc clears, Enter on no matches stays open, adding a host clears the filter.
- Render: a 40-host table at 120x30 with a filter shows the right rows and the title count.
- Live on the hub with fbsd16, t1 and t2 (bhyve VMs) and the hub itself: the Type column shows `vm` for the VMs and `metal` or `vm` for the hub as the hypervisor reports; a configured tag appears; `/` narrows by tag.
