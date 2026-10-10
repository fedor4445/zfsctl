# The Alerts screen: alert settings in the TUI — Design

Date: 2026-10-10

## Goal

Let the operator view and change the health-alert settings (`[notify]` in `config.toml`) from the hub's TUI, see whether the monitor is working, and send a test message, without editing the file by hand. Saving must never damage the rest of `config.toml`.

Today the alerts exist only as a hand-edited `[notify]` section. Add Host rewrites the whole config by serialising it, which drops comments and formatting; that is not acceptable for settings that can switch on email.

## Scope

- A new sidebar section **Alerts**: the saved settings, the monitor's live status, the recent events.
- `e` edit the settings in a form; `t` send a real test message; `p` preview the report; `r` refresh.
- A **section-only save**: only the `[notify]` section of `config.toml` is replaced (or appended), everything else is kept byte for byte, with a one-file backup and an atomic write.

Non-goals (YAGNI): editing the notify hook command (`[daemon] notify_command`) here (it is a shell command, shown read-only; it belongs in the file), editing hosts or the theme here, per-host alert settings, acknowledging or silencing alerts, moving Add Host to the same save method (worth doing, not in this change), a TUI on a host other than the hub (it only exists there).

## Decisions made while brainstorming

1. **A new sidebar section** named Alerts (last in the list), not a key under Hosts.
2. **The hook is read-only** in the TUI.
3. **Section-only text save** with `config.toml.bak`, not a serialise-everything rewrite.

## The screen

`Section::Alerts`, no bottom metrics panel (it uses the whole body). Three blocks, top to bottom:

**Settings** (as saved in the file):
```
Email to        someone@example.net
Mail command    msmtp -a default -t        (default)
Mail from       zfsctl@<hub>               (default)
Check every     5 minutes                  (default)
Daily report    07:00                      (default)  
Remind every    6 hours                    (default)
Hook command    curl -s ...                (edit in config.toml)
```
Unset keys show the default with `(default)`, dimmed. If neither an address nor a hook is set the line `Monitor: off — set an email address to turn alerts on` is shown instead of the status block.

**Status**: `Monitor on (every 5 min, report at 07:00)`; `Daemon running` or `Daemon not running` (from the heartbeat, as the Jobs panel does); `Last report sent 2026-10-10` or `never` (from `<state>/notify.json`); the number of hosts currently alerted is not known to the TUI (the daemon holds it in memory) and is not shown.

**Recent events**: the last 15 lines of `<state>/health-events.jsonl` (newest first): time, host, kind (`alert`, `escalation`, `reminder`, `recovery`, `daemon-started`), text.

Keys (also in the hint bar and Help):
- `e` edit settings (form below).
- `t` send a test message. A confirmation (two keys, as everywhere) names the recipient and the command; on yes it runs `notify_test` with the **saved** settings in a worker thread (it can take up to 30 seconds) and the result (`mail: ok` / `mail: failed: ...`) appears as the status message. With nothing configured it says so instead of asking.
- `p` preview the report: probes the hosts in a worker thread (up to a minute for an unreachable host) and shows `zfsctl report`'s text in an info pop-up. Nothing is sent or recorded.
- `r` reloads the screen's data. The screen also reloads itself every 5 seconds while it is open and after a save.
- Up/Down scrolls the events list.

## The edit form

Fields, in order, each pre-filled with the saved value (blank when unset):

| Field | Meaning | Blank means |
|---|---|---|
| Email to | one address or several, comma separated | email off |
| Mail command | shell command that receives the message on stdin | default `msmtp -t` |
| Mail from | the From address | default `zfsctl@<hub>` |
| Check every (minutes) | 1–1440 | default 5 |
| Daily report at | `HH:MM`, or `off` | default 07:00 |
| Remind every (hours) | 0–8760, 0 = never | default 6 |

Enter on the last field validates with `NotifyConfig::resolve` (the daemon's own check, so the form refuses exactly what the daemon would refuse, with the same message, naming the field) and keeps the form open on an error. On success a two-key confirmation shows what will be written (`Save alert settings? Email to someone@example.net, check every 5 minutes, report at 07:00. The daemon applies them within seconds.`). Declining returns to the form with what was typed (the behaviour every irreversible form has). Esc discards.

Saving an address the operator has not used before sends nothing by itself; `t` is the explicit way to check it.

## The save: section-only, atomic, with a backup

`config::set_notify_section(text: &str, section: &str) -> Result<String, String>` (pure):
- The new section is rendered from the form: `[notify]` followed by one `key = "value"` line per **set** key (blank fields write nothing, so a default stays a default). Strings are written with TOML escaping.
- If `text` has a `[notify]` table header line (leading and trailing spaces and a trailing `# comment` allowed), the header line is kept as it is and, up to the next table header line or the end of the file, only the lines that set this screen's own keys (`mail_to`, `mail_command`, `mail_from`, `check_interval_minutes`, `report_time`, `remind_hours`) are replaced by the new key lines, written right after the header. Comments, blank lines and any other key in the section stay. Otherwise the section is appended after a blank line.
- Refused with a message telling the operator to edit by hand: a `notify` key written as a dotted key (`notify.mail_to = ...`) or an inline table (`notify = {`), or more than one `[notify]` header.
- The result is parsed before anything is written and refused unless (a) its `[notify]` equals what was asked and (b) the whole file, parsed as data with `notify` removed, is identical to the old file's: a surprise in a string, an unknown table or any other section is never written.

`config::save_notify_section(path, section)`: follows a symlinked `config.toml` (the link stays a link), reads the file (a missing file is treated as empty; a non-empty file that reads as empty is refused), computes the new text, then: copies the current file to `<real>.bak` (overwriting the previous backup), writes the new text to a private `<real>.tmp.<pid>` created exclusively in the same directory with the original mode, flushes it to disk, gives it the original owner and group, refuses if the file changed (size or modification time) while it worked, renames it over the file and flushes the directory. A failure at any step leaves the original untouched and says which step failed. Nothing but `[notify]` ever changes.

The daemon notices the new modification time within a second and applies the settings (its monitor within 30 seconds); a settings change keeps the alert state and the retry queue.

## Wiring

- `App` gains `config_path: PathBuf` (passed from `run_app`, which has it) and an `AlertsView` (settings loaded from the file, status, events), loaded when the section is selected and by the 5-second refresh.
- The file is read with `Config::load`; a file that does not parse shows its error in the screen (the saved-settings block says `config.toml could not be read: ...`) and `e` is disabled until it is fixed.
- The test and the preview run on worker threads and report through the existing `WorkerMsg` channel; a result for a screen the user has left is shown as a status message, never lost.
- `notify_cli::notify_test` and `report_text_with` are reused as they are.

## Safety and privilege

No new privileged command and no sudoers change. The TUI already runs as root on the hub and already writes `config.toml` (Add Host). The only outward-facing actions are the explicit test message (`t`, after a confirmation) and the saved settings the daemon then uses; the preview sends nothing.

## Testing

- **`set_notify_section`** (pure): replaces an existing section and keeps everything before and after byte for byte (including comments and the next table); appends when absent; a section at the end of the file and one in the middle; a trailing comment on the header; CRLF files; a file without a final newline; refuses dotted keys, an inline table and two headers; the output always parses back to a `Config` whose `notify` equals what was asked for; blank fields write nothing; TOML escaping of a command containing quotes and a backslash.
- **`save_notify_section`** (temp dir): the backup holds the old bytes; the new file keeps the permissions; a rename failure (read-only directory) leaves the original intact; an unparsable result is refused with the file untouched.
- **Form:** pre-fill from the saved settings; each bad value is refused with the daemon's message and the form stays open; blank means unset; declining the confirmation returns the form with its values; accepting writes and reloads the screen.
- **Screen/render:** settings with `(default)` markers, the monitor-off line, status lines, the events list (newest first, cut at 15); the hook is read-only; narrow terminals cut with an ellipsis; `e` disabled when the file does not parse.
- **Keys:** `t` asks first and names the recipient; with nothing configured it says so; the result arrives as a status message; `p` shows the preview in a pop-up; unadvertised keys stay inert (the hint-bar drift guard covers the new section).
- **Live on the hub** (with the user's agreement before anything is sent): change the check interval from the TUI and see the daemon log `health monitor settings changed`; the hub's real config keeps its comments and its other sections byte for byte (diff against the backup shows only the `[notify]` lines); `t` sends one test message; `p` previews; restore the config afterwards.

## Live results (2026-10-10)

- On the hub (the rc daemon untouched, the TUI run from the build directory): the Alerts screen showed the saved settings with `(default)` marks, `Monitor on (every 5 min, report at 07:00)`, the last report date and the daemon-start event, matching `config.toml` and `/var/db/zfsctl`.
- `e`, Check every = 3, `y` + Enter: the diff against the backup was exactly one added line (`check_interval_minutes = 3`) with comments and every other section identical; `config.toml.bak` held the previous bytes; the file kept its owner, group and mode (`root wheel 0644`); the daemon logged `health monitor settings changed`; the screen then read `Monitor on (every 3 min, ...)`.
- `e` then Esc wrote nothing. `p` showed the report preview in a pop-up (all four hosts) and sent nothing.
- `t` named the recipient (`someone@example.net`) and the command, sent only after `y` + Enter, and reported `Test message sent: the mail command accepted it`.
- The hub's config was restored byte for byte and the backups removed.
- Not tested live: saving over a symlinked config or a config owned by another user (unit-tested for the symlink; the chown path needs root and a different owner), and a save while another editor changes the file (the guard has no deterministic test).
