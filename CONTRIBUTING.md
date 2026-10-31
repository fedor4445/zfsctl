# Contributing to zfsctl

Thank you for looking. zfsctl runs privileged commands on storage, so changes are reviewed slowly and carefully. This page
says what makes a change easy to accept.

zfsctl is built for FreeBSD only, and for FreeBSD hosts that it manages over ssh. Changes that add other operating systems
are out of scope.

## Before you start

- **Open an issue first** for anything larger than a typo or a small fix. Say what you want to change and why. It saves you
  from writing code that cannot be merged.
- Read the [book](docs/book/README.md) and the README. Many choices that look odd (two-step confirmations, typed names
  before destroying things, refusing instead of guessing) are deliberate. The project's first rule is that it must not
  lose data.
- **Security problems** go through [SECURITY.md](SECURITY.md), never into a public issue.

## What a pull request needs

- **One logical change per pull request.** Small is better.
- **Tests.** `cargo test` must pass, and new behaviour needs tests. The unit tests use fakes for `zpool`, `zfs` and ssh, so
  they run on any machine.
- **Try it for real if it touches storage.** Anything that runs `zpool`, `zfs`, `bectl`, `service`, ssh or sudo has to be
  tried on a **scratch pool or a throwaway VM** before review, because fakes cannot show how the real tools behave. Say in
  the pull request what you ran, on which FreeBSD version, and what you saw. If you could not test it, say that instead.
  Never test on a pool that holds data you care about.
- **Checks that CI runs:** `cargo test`, `cargo clippy` and `cargo deny check`. Run them locally first. Clippy warnings do
  not fail the build yet, but your change should not add new ones.
- **Documentation.** If a screen, key, job field or command changes, update the book chapter and the README. A test
  (`book_check`) keeps the book and the program in step, so it will fail if you forget.

## Rules the code follows

- **Destructive actions need a deliberate second step.** A lone `y` or Enter must never destroy, overwrite, roll back or
  reconfigure anything. Irreversible actions ask for the typed name and say exactly what will be lost. Prefer refusing
  with a reason over doing something broad.
- **No `unsafe`.** The crate has `#![forbid(unsafe_code)]`. If you think you need it, open an issue to discuss it.
- **Every command run on a managed host is listed in `src/exec/privilege.rs`.** That table produces the sudoers entry
  (`zfsctl print-sudoers`) and the host check. A new command must be added there, with its feature, or it will not be
  allowed on remote hosts.
- **Arguments are quoted, never pasted into a shell string.** Use the helpers in `src/exec/`.
- **New dependencies need a reason.** Say in the pull request why the standard library or an existing crate is not enough.
  `cargo deny check` must still pass.
- **Rust only.** Do not add scripts in other languages to the build or the tests.
- **Do not run `cargo fmt` over the whole tree.** It would rewrite thousands of lines and bury your change. Format only
  what you touch, in the style around it.

## Using AI tools

zfsctl itself was built with Claude, and AI-assisted contributions are welcome under the same conditions as any other:

- Tell us in the pull request if an AI tool wrote a significant part of it.
- **You are responsible for the change.** You must understand every line you submit and be able to explain it. A pull
  request that nobody can explain will be closed.
- AI review may be used on pull requests as a first pass. A person makes the decision to merge.

## Review

The maintainer reviews and merges pull requests by hand. Expect questions about what happens when a command fails, when
the output looks different on another FreeBSD version, and when the user presses the wrong key. Please be patient: the
review is slow on purpose.

## Licence

zfsctl is released under the [BSD 2-Clause licence](LICENSE). By submitting a pull request you agree that your
contribution is released under the same licence.

## Questions

Write to hello@zfsctl.com.
