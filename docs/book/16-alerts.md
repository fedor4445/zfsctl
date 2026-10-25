# Alerts and the daily report

## What you will learn

- What the health monitor watches, and when it speaks up.
- How to turn alerts on, from the Alerts screen or the config file.
- How to send mail your own way, or use a hook for Telegram, ntfy and friends.

## What it does

When the daemon is running and at least one way of reaching you is configured, it **checks every host** every five minutes with the same logic as the Hosts table, so an alert and the table never disagree. It tells you:

- **An alert** when a host is bad (yellow or red) on **two checks in a row**; a single blip never wakes anyone. Subject: `[zfsctl] fbsd16: tank DEGRADED`.
- **An escalation** if it gets worse (yellow to red).
- **A reminder** every six hours while it stays bad.
- **A recovery** when the host is fine again on two checks.
- **A daily report** at 07:00: a table of all hosts, what needs attention, and what happened since yesterday (jobs run and failed, scrubs finished, alerts and recoveries). The disks that have a SMART problem (each with what it is for when that is known: `ada4 (cache of tank) SSD wear 87%`) and the hosts whose NFS or Samba is stopped while they have shares are listed under what needs attention (see chapter 6); there are no separate alerts for them. It is sent even on a perfect day, so that silence means the sender is broken, not the hosts. When any host has a `tag` (chapter 7), the report starts with a **GROUPS** block, one line per tag (`web (12)  1 critical, 2 warning`, or `ok`; groups with a problem first, hosts without a tag last), and the attention lines name the group (`[web] fbsd16: tank DEGRADED`). An alert's email says `Group:    web` in its body, and a group may have its own recipients (`[groups.web.notify] mail_to`, chapter 21), so each team gets the alerts about its own hosts and the daily report stays with you; the subject lines stay as they were, so mail filters keep working.

Failed scheduled jobs also reach you, through the same channels.

### Unprotected data

The monitor also judges every dataset on every host: is it protected, that is, covered by current snapshot and copy jobs (chapter 6 explains the rules and the three levels)? When a dataset becomes unprotected you get one email **per host**, listing every new dataset (a new pool with six datasets gives one email, not six), and when it is protected again a recovery email. These emails are about your data only: the operating system's datasets (OS data) stay on the Dashboard and in the daily report, and get no email of their own. A reminder comes at most once a day (the larger of the Remind every setting and 24 hours), since the daily report lists them anyway. A daemon restart sends the alerts again for datasets that are still unprotected, again one email per host. The daily report has a section for it:

```text
UNPROTECTED DATA
  local:tank/photos (your data): no snapshot job (for 3d)
  fbsd16:tank/vm (your data): no copy job (for 1d)
  fbsd16:zroot/var (OS data): no snapshot job
```

When all is well the section says `All datasets are protected.`, but only when every configured host was checked. A host that is unreachable, or whose dataset list is missing or more than two days old, is named instead: `All datasets on 2 hosts are protected; not checked: t2 (unreachable), omnios (no dataset list for 3d)` (or, under a list of datasets, `Not checked: ...`). Nothing is ever called protected on a host zfsctl could not see. The `zfsctl report` preview says the same. The setting **Email** (Settings, group Data protection, or `email` in `[protection]`) picks the timing: `immediate` (the default: the emails above plus the report section), `report` (only the report section, still with how long each dataset has been unprotected; the `zfsctl report` preview has no such memory and leaves the "for 3d" out) or `off` (the Dashboard only; no email at all about this check).

### Data safety in the report

Under the unprotected data, the report has a **DATA SAFETY** block: the same picture as the Dashboard's data-safety line (chapter 6), for the five datasets whose copies are weakest, one line each, and a count of the rest:

```text
DATA SAFETY
  local:tank/photos: tolerates 1 failure, oldest copy 3 h
  fbsd16:tank/vm: tolerates no failure, oldest copy 5 d (b2:bkt/vm is not current)
  and 2 more with copies
```

`tolerates N failures` is how many separate failure domains (hosts, or hosts sharing a `site`, and cloud remotes) can be lost while the data survives. `oldest copy` is the age of the data in the least fresh place, and a place whose copy job has stopped or never ran is named as not current. With no copy job at all it says `No dataset has a copy yet`. The block follows the same **Email** setting as the unprotected data: with `off` (everywhere) there is neither. It is advice, not an alert: nothing is mailed about it between reports.

## The Alerts screen

```screen
┌zfsctl────────────────┐┌Alerts — daemon running (pid 39561)───────────────────────────────────────────────────────────┐
│Dashboard             ││Settings (config.toml)                                                                        │
│Hosts                 ││Email to       you@example.com                                                             │
│Pools                 ││Mail command   msmtp -a default -t                                                            │
│Datasets              ││Mail from      zfsctl@fb   (default)                                                          │
│Snapshots             ││Check every    5 minutes   (default)                                                          │
│Disks                 ││Daily report   07:00   (default)                                                              │
│Shares                ││Remind every   6 hours   (default)                                                            │
│Boot Envs             ││Hook command   (not set)   (edit in config.toml)                                              │
│Jobs                  ││                                                                                              │
│Cloud                 ││Monitor on (every 5 min, report at 07:00)                                                     │
│Alerts                ││Last report sent 2026-10-24                                                                   │
│Settings              ││                                                                                              │
│                      ││Recent events                                                                                 │
│                      ││10-24 21:55  fb  daemon-started  zfsctl daemon started                                        │
│                      ││10-24 21:06  fb  daemon-started  zfsctl daemon started                                        │
│                      ││10-24 20:38  fb  daemon-started  zfsctl daemon started                                        │
│                      ││10-24 20:35  ub24  alert  2 datasets not protected                                            │
│                      ││10-24 20:35  omnios  alert  40 datasets not protected                                         │
│                      ││10-24 20:35  local  alert  11 datasets not protected                                          │
│                      ││10-24 20:35  fbsd16t2  alert  5 datasets not protected                                        │
│                      ││10-24 20:35  fbsd16t1  alert  6 datasets not protected                                        │
│                      ││10-24 20:35  fbsd16  alert  4 datasets not protected                                          │
│                      ││10-24 20:30  fb  daemon-started  zfsctl daemon started                                        │
│                      ││10-24 19:15  ub24  alert  2 datasets not protected                                            │
│                      ││10-24 19:15  omnios  alert  40 datasets not protected                                         │
│                      ││10-24 19:15  local  alert  11 datasets not protected                                          │
│                      ││10-24 19:15  fbsd16t2  alert  5 datasets not protected                                        │
│                      ││10-24 19:15  fbsd16t1  alert  6 datasets not protected                                        │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
└──────────────────────┘└──────────────────────────────────────────────────────────────────────────────────────────────┘
e Edit settings  t Send test  p Preview report  ↑↓ Scroll  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

The top block shows the saved settings, with `(default)` beside anything you have not changed. Then comes the status: whether the monitor is on, when the last report went, and below it the recent events, newest first. Keys:

| Key | What it does |
|---|---|
| `e` | edit the settings (a form; saved into `config.toml` after a yes/no question) |
| `t` | send a real test message to your address (after a yes/no question that names the recipient) |
| `p` | preview the report as it would be sent now; nothing is sent |
| `Up` / `Down` | scroll the events |
| `r` | refresh |

The form has these fields: **Email to** (one or several addresses, separated by commas), **Mail command**, **Mail from**, **Check every (minutes)**, **Daily report at** (`HH:MM` or `off`) and **Remind every (hours)** (`0` for never). A blank field means the default. The form refuses what the daemon would refuse, with the same message.

> **Safety:** saving changes only the `[notify]` section of `config.toml`. The rest of the file, with its comments, stays byte for byte, a backup is kept as `config.toml.bak`, and the write is atomic. The daemon applies the change within seconds.

## Choose how mail is sent

zfsctl has no mail client of its own, on purpose. It writes a complete message (headers and body) to the **standard input of a command you choose**, so use what you already have:

| You use | Mail command |
|---|---|
| msmtp (the default) | `msmtp -t` or `msmtp -a default -t` |
| the system's sendmail-compatible | `sendmail -t` |
| your own script | any command that reads a message on stdin |

`msmtp` reads the recipient from the message (`-t`) and your account from `~/.msmtprc`; zfsctl makes sure the daemon sees the real home directory of the user it runs as, so that file is found. A mail command that fails, or takes more than 30 seconds, counts as a failed delivery; zfsctl retries on each following check, up to six times.

## Telegram, ntfy and anything else: the hook

If you would rather be told some other way, set a **hook** in `config.toml`; it needs no email at all:

```toml
[daemon]
notify_command = "curl -s -d \"$ZFSCTL_MESSAGE\" ntfy.sh/my-zfs-alerts"
```

The hook runs through `sh -c` with these environment variables: `ZFSCTL_KIND` (`health-alert`, `health-recovered`, `health-reminder`, `health-report`, or a job kind), `ZFSCTL_HOST`, `ZFSCTL_JOB`, `ZFSCTL_RESULT`, `ZFSCTL_MESSAGE` (the subject) and `ZFSCTL_BODY` (the full text, also on stdin). Nothing from a host's output is ever put on the command line, so a strange pool name cannot run as code. The hook is killed after 30 seconds. Telegram is a one-line `curl` to the Bot API in the same way; the hook is shown on the Alerts screen but edited in the file.

> **Safety:** if only a hook is configured (no email), the monitor runs and uses it.

## From the command line

- `zfsctl notify-test` sends a short test message through every configured channel and prints one line each (`mail: ok`, `hook: ok`); exit code 0 means everything worked.
- `zfsctl report` prints the report as it would be sent now and sends nothing.

Both need to run as root on the hub, like the daemon.

## Try it

Open Alerts, press `p` to preview the report, press `e`, fill in your address and the mail command you use, save, and press `t` to send a test message. If it does not arrive, run `zfsctl notify-test` in a shell and read what it says; the next chapters cover common causes.

The theme, the Dashboard thresholds and the cloud caps are edited on the Settings screen, the next one in the sidebar.

## Where next

- [Recipes](17-recipes.md)
- [Troubleshooting](19-troubleshooting.md): "no mail arrives".
