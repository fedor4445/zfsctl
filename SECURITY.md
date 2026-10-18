# Security policy

zfsctl manages ZFS storage, and on the machines it manages it runs commands as root (through `sudo` rules you install
yourself). A vulnerability here can mean data loss, so please report one privately rather than in a public issue.

## Reporting

Use GitHub's private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**.
Please include the version (`zfsctl --help` shows it), the operating systems involved, and the steps to reproduce.

You can expect an acknowledgement within a week. Fixes go into the next release, and the report is credited unless you
prefer otherwise.

## What is in scope

- A way to run a command that zfsctl does not intend to run, or to run one with more privilege than the sudoers rules it
  prints (`zfsctl print-sudoers`) grant.
- A way to read or leak a secret zfsctl handles (an encryption passphrase, a Samba password, a restic password file path
  with its content).
- A way to destroy or overwrite data without the confirmations the screens ask for.

## Supported versions

Only the latest release is supported while the project is young.
