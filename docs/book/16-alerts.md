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
- **A daily report** at 07:00: a table of all hosts, what needs attention, and what happened since yesterday (jobs run and failed, scrubs finished, alerts and recoveries). It is sent even on a perfect day, so that silence means the sender is broken, not the hosts.

Failed scheduled jobs also reach you, through the same channels.

## The Alerts screen

```screen
┌zfsctl────────────────┐┌Alerts — daemon running (pid 98986)───────────────────────────────────────────────────────────┐
│Hosts                 ││Settings (config.toml)                                                                        │
│Pools                 ││Email to       you@example.com                                                             │
│Datasets              ││Mail command   msmtp -a default -t                                                            │
│Snapshots             ││Mail from      zfsctl@fb   (default)                                                          │
│Disks                 ││Check every    5 minutes   (default)                                                          │
│Shares                ││Daily report   07:00   (default)                                                              │
│Boot Envs             ││Remind every   6 hours   (default)                                                            │
│Jobs                  ││Hook command   (not set)   (edit in config.toml)                                              │
│Alerts                ││                                                                                              │
│                      ││Monitor on (every 5 min, report at 07:00)                                                     │
│                      ││Last report sent 2026-10-10                                                                   │
│                      ││                                                                                              │
│                      ││Recent events                                                                                 │
│                      ││10-10 15:21  fb  daemon-started  zfsctl daemon started                                        │
│                      ││10-10 15:04  fb  daemon-started  zfsctl daemon started                                        │
│                      ││10-10 14:51  fb  daemon-started  zfsctl daemon started                                        │
│                      ││10-10 14:11  fb  daemon-started  zfsctl daemon started                                        │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
│                      ││                                                                                              │
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

The hook runs through `sh -c` with these environment variables: `ZFSCTL_KIND` (`health-alert`, `health-recovered`, `health-reminder`, `health-report`, or a job kind), `ZFSCTL_HOST`, `ZFSCTL_RESULT`, `ZFSCTL_MESSAGE` (the subject) and `ZFSCTL_BODY` (the full text, also on stdin). Nothing from a host's output is ever put on the command line, so a strange pool name cannot run as code. The hook is killed after 30 seconds. Telegram is a one-line `curl` to the Bot API in the same way; the hook is shown on the Alerts screen but edited in the file.

> **Safety:** if only a hook is configured (no email), the monitor runs and uses it.

## From the command line

- `zfsctl notify-test` sends a short test message through every configured channel and prints one line each (`mail: ok`, `hook: ok`); exit code 0 means everything worked.
- `zfsctl report` prints the report as it would be sent now and sends nothing.

Both need to run as root on the hub, like the daemon.

## Try it

Open Alerts, press `p` to preview the report, press `e`, fill in your address and the mail command you use, save, and press `t` to send a test message. If it does not arrive, run `zfsctl notify-test` in a shell and read what it says; the next chapters cover common causes.

## Where next

- [Recipes](17-recipes.md)
- [Troubleshooting](19-troubleshooting.md): "no mail arrives".
