# Settings screen: design

Date: 2026-10-17. Status: draft for approval.

## Why

Changing anything in `config.toml` today means ssh to the hub, edit as root, remember the key names and valid
values, then restart the daemon. The user finds all of that tedious. Hosts (Add Host) and the notify settings
(Alerts screen) are already edited in the TUI and saved safely; the rest is not: the Dashboard thresholds
(`[dashboard]`), the cloud plan sizes (`[cloud.caps]`), the theme and the auto-refresh interval (`[ui]`).

The user rarely changes these today ("so far I haven't changed much"), so this is a convenience, not a big
feature. Keep it small. Jobs, backups and cloud mirrors, the settings they expect to change most, already have
their own forms and are out of scope.

## What

A new sidebar section **Settings** (placed last, after Alerts) listing the remaining settings in groups. The user
chose this over editing each setting on the screen where it shows up, and over opening `$EDITOR` on the file.

```
Display
  Theme                solarized-dark
  Auto-refresh         10 s
Dashboard thresholds
  Memory warn / crit   90 / 97 %
  Swap warn / crit     50 / 90 %
  CPU warn             95 %
  Scrub warn / crit    35 / 90 days
  Scrub ignore         scratch
Cloud caps
  b2                   10G
  zfsctl-aws           5G
```

- Enter on a row opens a small edit form for that value (or pair of values); Enter again saves. Rows that are on
  their default show it dimmed, as the Alerts screen does. An empty value (or a key for "reset") returns the
  setting to its default by removing the key from the file.
- Cloud caps: one row per rclone remote known to zfsctl (the Cloud screen's list) plus any remote that already
  has a cap in the file; setting a size adds it, clearing removes it.
- A save takes effect at once in the running TUI: the theme repaints, the refresh interval changes, the Dashboard
  and Cloud screens use the new numbers on their next draw. No daemon restart: the daemon does not read these
  sections (it reads `[notify]` and `[daemon]`).

## Rules

- Validation is the file's own: a warn/crit pair that does not make sense (warn not below crit, zero, over 100) is
  refused in the form with a message, not silently replaced by the defaults the loader would use. Cap sizes use
  the loader's format (digits, optional decimal point, optional K/M/G/T). Theme is one of the four names.
  `auto_refresh_secs` is a number, `0` turns it off.
- Writing reuses the safe write the Alerts and Add Host saves share (`rewrite_config` in `src/config.rs`): backup
  to `config.toml.bak`, private temp file, same owner and mode, rename, and the guard that nothing outside the
  edited section changes (comments, other sections, host list stay as they are). That helper and its
  "only this section changed" guard are now notify-specific and need generalising to the sections above.
- Unreadable or unparsable `config.toml`: the screen says so and edits nothing, as Alerts does.
- The TUI runs as the user who can write the file (root on the hub). If the file is not writable the save fails
  with the reason; nothing is lost.

## Decisions made while writing (my calls, flagged for the user)

- **Reload:** the running TUI keeps no single in-memory `Config`: the theme and refresh interval are resolved once in
  `run_app`, and the Cloud caps are re-read from the file whenever the Cloud list loads (`state.rs`). The plan adds a
  small reload: after a successful save, re-read the file, then apply theme (`theme::resolve` then `bright()`),
  refresh interval, and the Dashboard thresholds. Settings also re-reads the file just before writing, so a hand
  edit made after the screen opened is not overwritten, and the section guard refuses the save if the file
  changed in a way it does not understand. A hand edit made while the TUI runs is picked up the next time the screen
  loads (leaving and re-entering Settings, or `r`), not continuously.
- **Editing the file:** use `toml_edit` (already pulled in by the `toml` crate, so no new code is trusted)
  instead of extending the line-based `[notify]` helper, which cannot cope with sub-tables, inline tables or
  trailing comments. Same guard as today: after the edit, everything outside the edited section must be equal as
  data; the result must parse as `Config` and say what was asked; an empty section left by a reset is removed.
  A missing section is appended at the end of the file. `[cloud]` and `[cloud.caps]` are one unit for the guard.
  `rewrite_config` (backup, temp file, owner and mode, rename) is kept as is.
- **Pairs:** the form shows both numbers; changing either writes both keys (so what you see is what is in the file).
  A pair left equal to its defaults is removed from the file, which is how a reset works. Warn below crit is checked
  on the pair shown in the form. CPU warn is 1 to 100; auto-refresh is 0 (off) or 1 to 3600 seconds.
- **Scrub ignore:** comma-separated names, not checked against live pools (a pool may be offline or not yet created).
- **Theme:** the default is "not set" (shown as the default, dimmed): `ansi`, or `mono` when `NO_COLOR` is set.
  Choosing `ansi` writes it explicitly; a reset removes the key. The theme is a value in the app's state, so it
  repaints at once; a note appears when a Solarized theme is chosen without truecolor, as at startup.
- **Cap rows:** the remotes are the ones the Cloud screen already has in memory plus any remote already capped in the
  file. Settings never calls rclone itself; if the Cloud list has not loaded, only the capped ones are listed and
  a line says "open Cloud once to list your remotes". Caps belong to the hub's `config.toml` (the file the TUI
  runs on), not to a host.

## Not in scope (anti-goals)

- No new way to edit hosts, notify settings, jobs: they stay where they are. The notify settings do not appear
  in Settings (my call: one place for each setting; the user was not asked).
- The hook command (`[daemon] notify_command`) stays "edit in config.toml": it is a shell command run by the
  daemon, which does not reload `[daemon]`, and a typo in it silently breaks every alert.
- No restart of the daemon from the TUI. No editing of ZFS properties (ashift etc.).
- It must never be possible to write a file that no longer parses.

## Left to the builder

Key bindings and form layout (follow the existing forms and the hint bar), how the group headers are drawn,
whether the theme row previews the theme before saving (nice to have), screen names in the book and README, the
book chapter (a new section in chapter 6 and an entry in chapter 21), tests in the repo's usual style.

## Done means

Spec, plan, implementation with tests, whole-branch review, a live check on the hub (change a threshold and a
cap, watch the Dashboard and Cloud screens change, confirm `config.toml` keeps its comments and hosts and a
`.bak` exists), the book updated with a new capture, merged and installed.
