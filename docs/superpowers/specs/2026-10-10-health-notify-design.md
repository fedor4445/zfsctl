# Health alerts and a daily report by email — Design

Date: 2026-10-10

## Goal

Tell the operator when a managed host or pool needs attention, and send a daily health report covering every host, without anyone having the TUI open. Delivery is by email through the msmtp already set up on the hub, and through the existing notify hook (which is how Telegram works today with a one-line `curl`).

Today the daemon only runs jobs. Its `notify_command` runs when a scheduled job fails. Host and pool health (the Hosts table's states) exist only in the TUI, and nothing is ever sent unless a job fails.

## Scope

- A **health monitor** in the daemon: probes every host on an interval, alerts when a host turns bad, when it escalates, and when it recovers, and reminds while it stays bad.
- A **daily report** at a set time, for all hosts: a table, what needs attention, and the events since the last report.
- **Delivery** through two channels, both optional: email (`mail_command`, default `msmtp -t`) and the existing `notify_command` hook. Job-failure notifications use the same channels.
- Two read-only commands: `zfsctl notify-test` (sends a test message and says which channel worked) and `zfsctl report` (prints the report that would be sent now, sends nothing).

Non-goals (YAGNI): a built-in Telegram or SMS client (the hook covers it), per-host or per-pool thresholds (the Hosts table's 80% and 90% apply), acknowledging or silencing alerts, HTML mail, alert state surviving a daemon restart, monitoring the daemon from outside (a dead daemon sends nothing; a missing daily report is that signal, and the TUI shows the heartbeat), new privileged commands.

## Decisions made while brainstorming

1. **Same logic as the Hosts table.** The monitor uses `host_health::probe_host` and `assess`, so an alert and the TUI never disagree. Thresholds, reasons and levels are the table's.
2. **Alerts on change, not on every probe.** A blip must not page anyone: a host has to be bad on two consecutive probes (about 10 minutes at the default interval) before the first alert. Recovery is likewise two consecutive good probes.
3. **Email goes through the operator's own command**, not a built-in SMTP client. zfsctl writes the full message (headers included) to the command's stdin, so `msmtp -t` reads the recipient from it.
4. **Telegram through the hook for now.** The hook receives the alert and report text, so `curl` to the Bot API works without zfsctl handling a bot token.
5. **Both alerts and the report go to the same recipients.**

## Configuration

A new `[notify]` section; the existing `[daemon] notify_command` keeps its name and meaning (the hook), so existing configs are unchanged.

```toml
[notify]
mail_to = "you@example.com"          # one address or several, comma separated; unset = no email
mail_command = "msmtp -a default -t" # default "msmtp -t"; zfsctl writes To/From/Subject/Date and the body on stdin
mail_from = "zfsctl@hub"             # optional; default zfsctl@<the hub's hostname>
check_interval_minutes = 5           # default 5, at least 1
report_time = "07:00"                # local time on the hub; default 07:00; "off" disables the report
remind_hours = 6                     # repeat an unresolved alert this often; default 6, 0 = never
```

- The monitor runs only when at least one channel is configured (`mail_to` or `notify_command`); with none, the daemon does no probing and no extra ssh.
- `mail_to` is checked at load: every address must contain `@` and no whitespace, control character or comma inside an address (a newline there would inject headers). A bad `[notify]` section is reported in the daemon log and the previous settings stay, as for hosts today.
- The settings reload when `config.toml` changes, like hosts and the hook.
- Both commands are the operator's own shell text (same trust as `notify_command` today). Message text reaches them only on stdin and in environment variables, never on the command line.
- **PATH and HOME.** The rc-started daemon has a minimal PATH and `HOME=/`. Delivery commands run with the package directories appended to PATH (as `LocalRunner` does) and the effective user's real home (`with_account_home`), so `msmtp` is found and reads `~/.msmtprc`.

## The monitor

`src/daemon/health.rs`, one thread started by `daemon::run`, stopped by the same stop flag.

**Each tick** (every `check_interval_minutes`): take a snapshot of `shared.hosts`, probe every host in parallel with `probe_host_guarded(runner, false)` (the same two calls the Hosts table makes: `zpool list` and `zpool status`; nothing new on any host), and compute each host's assessment with `assess`, including the job-failed input from the jobs file and the history (an enabled job of that host whose latest run failed). A probe that has not answered after 60 seconds counts as "no answer" (warning), as in the TUI.

**State per host:** the last level and reason, how many consecutive probes were bad or good, and the alert state (none, or alerted at a level with the time of the last message).

**Rules** (a pure `Tracker`, tested without a clock or threads):
- A host is *bad* at level Warning or worse (Critical and Disconnected count as Critical).
- First alert: bad at the same level on two consecutive probes. Subject `[zfsctl] fbsd16: tank DEGRADED`.
- Escalation (Warning to Critical) alerts after two consecutive probes at the new level. A change of reason at the same level does not alert; the new reason shows in the next reminder.
- Reminder: while an alert is active, every `remind_hours`, `[zfsctl] fbsd16: still: tank DEGRADED (for 6h)`. `0` disables reminders.
- Recovery: two consecutive Ok probes after an alert gives `[zfsctl] fbsd16: recovered`, and says how long it was bad. A host that never alerted never sends a recovery.
- Probing state (first probe in flight) never counts either way.
- A daemon restart forgets the state: a host that is still bad alerts again once, two probes after the start. Each daemon start is also recorded as an event for the report.

**Alert body** (plain text): host, address, time, level and reason, the host's table row, and its pools (state, capacity, errors, last scrub).

**Event log.** Alerts, recoveries, reminders and daemon starts are appended to `<state>/health-events.jsonl` (time, host, kind, text), trimmed to the last 2000 lines, so the report can list them.

## The daily report

Sent at `report_time` (hub local time) by the same thread. `<state>/notify.json` records the date of the last report: if the daemon was down at the report time, the report is sent once when it comes back (the same day), and never twice in a day.

Subject: `[zfsctl] daily report: 3 ok, 1 warning` (or `...: all 4 hosts ok`). Plain text:

1. **Summary line** and the hub's name and the time.
2. **Hosts table**: name, address, state, type, tag, used, pools, last scrub, jobs failed/total, one row per host (the columns of the Hosts table, as text).
3. **Needs attention**: each unhealthy host with its reason and how long it has been that way.
4. **Events since the last report** (24 hours if there was none): per job kind the runs and how many failed; every failed run with its job, host and message; scrubs finished (and any that found errors); the alerts, recoveries and daemon starts from the event log. Each section is cut at 40 entries with `(+N more)`.

An all-green day still sends a short report: silence then means the sender is broken, not the hosts.

## Delivery

`deliver(kind, subject, body)` sends to every configured channel; one channel failing does not stop the other.

- **Email.** zfsctl writes `From`, `To`, `Subject`, `Date`, `MIME-Version`, `Content-Type: text/plain; charset=utf-8` and `Content-Transfer-Encoding: 8bit`, a blank line, then the body, to `mail_command`'s stdin. Header values are single-line by construction (CR and LF removed) and subjects are ASCII (a host name with other characters shows them as `?` in the subject; the body keeps them). A command that fails, or runs past 30 seconds (killed), is a delivery failure.
- **Hook.** The existing `notify_command` is run with `ZFSCTL_KIND` set to `health-alert`, `health-recovered`, `health-reminder` or `health-report` (job failures keep their job kinds), `ZFSCTL_HOST`, `ZFSCTL_RESULT` (the level), `ZFSCTL_MESSAGE` (the subject), `ZFSCTL_BODY` (the full text) and the body on stdin. Existing hooks that read only the old variables keep working.
- **Job failures** also go to email, with subject `[zfsctl] job <id> failed on <host>` and the run's message as the body.
- **Failed delivery of an alert** is retried on each following tick, at most 6 times, then logged and dropped; the event log still records the alert. A failed report is retried the same way within its day. Every failure is logged with the command's exit status or timeout, never the message text.

## Commands

- `zfsctl notify-test`: sends a short test message through every configured channel and prints one line per channel (`mail: ok` or `mail: msmtp exited 78`, `hook: ok`). Exit 0 if all configured channels worked, 1 if any failed or none is configured. Running it sends real mail, so it is only ever run by the operator.
- `zfsctl report`: prints the report as it would be sent now (subject and body) and sends nothing; exit 0. It probes the hosts exactly as the monitor does.

Both run as root like the other admin commands, read `config.toml`, and print with `println!`.

## Safety and privilege

Read-only: no new command on any host and no new sudoers entry (`zpool` list and status through the existing runners). The only side effects are the configured commands, files in the daemon's own state directory, and log lines. Nothing in a host's output can reach a command line: message text is stdin and environment only.

## Testing

- **Tracker** (pure): two-probe confirmation, a single blip never alerts, escalation, reason change silent, reminder timing and `0`, recovery needs two good probes and only after an alert, a restart re-alerts once, probing state ignored.
- **Message composition:** every header present and single-line; a host name containing CR/LF or non-ASCII cannot add a header or break the subject; the body is verbatim; the `Date` header parses.
- **Config:** a config without `[notify]` loads and saves unchanged; defaults; `report_time` forms and `off`; a bad address (no `@`, whitespace, control character, comma inside) is refused with the reason; the section reloads.
- **Delivery** with a fake mail command (`cat > file`): To, Subject and body arrive on stdin; a failing or hanging command is a failure and the hook still runs; hook environment and stdin; retry up to 6 then drop; PATH and HOME are set for the command.
- **Report:** golden text for a mixed fleet (ok, warning, unreachable); events section counts and cuts; the once-per-day rule and the catch-up after a restart across midnight and across `report_time`; an all-green report.
- **Commands:** `notify-test` exit codes (none configured, one failing, all ok); `report` sends nothing.
- **Daemon:** the monitor thread starts only with a channel configured, stops on the stop flag, picks up hosts added by a config reload, and a hung probe does not stall the tick.
- **Live on the hub** (with the user's agreement to send mail): `zfsctl report` against fbsd16, t1, t2 and the hub; `notify-test` to the user's address through msmtp; a simulated problem (a throwaway host entry that cannot be reached) giving an alert after two probes and a recovery after it is removed; the report at a time set a few minutes ahead.

## Live results (2026-10-10)

- `zfsctl report` against the hub, fbsd16, t1 and t2: the table matched the Hosts screen; runs of a job deleted from `jobs.toml` showed as `?` (now `removed jobs`).
- `zfsctl notify-test` through the hub's msmtp (`msmtp -a default -t`, Gmail account): `mail: ok`, twice; both mails arrived in the user's inbox (the first went to the sender's own address before the recipient was settled).
- A foreground test daemon (throwaway state dir, `check_interval_minutes = 1`, a ghost host at an unreachable TEST-NET address): no mail after the first bad probe; the alert `[zfsctl] ghost: unreachable` after the second (about 80 s); after the ghost was pointed at a working VM, `[zfsctl] ghost: recovered after 2m` after two good probes; the daily report at its set time (`all 5 hosts ok`), exactly once; after a restart of the daemon no second report within the day (`notify.json` read back). The event log held the daemon starts, the alert and the recovery.
- The hub's real config was restored byte for byte and every temporary file removed. The installed daemon was never touched.
- Not tested live: a Telegram hook (only the hook environment is unit-tested), reminders after `remind_hours`, escalation from warning to critical, and a real hung host.
