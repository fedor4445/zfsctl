# Capture tools

The capture tool (`examples/capture.rs`, plain Rust, run with `cargo run --example capture`) takes the screens and command outputs the book shows. They are real: the tool drives the real TUI (or runs the real command) on the test hub and writes the text to `../captures/NAME.txt`. A chapter embeds a capture unchanged in a fenced block tagged `screen`; the book's tests check that the embedded text equals a capture file, so a capture is never edited by hand.

There is nothing to set up beyond the Rust toolchain. The TUI runs over `ssh -tt` with its size set by `stty`, and what it draws is read through a terminal emulator (the `vt100` crate, a dev-dependency), so a capture is the screen as a person would see it.

Examples (from the repository root):

```sh
cargo run --example capture -- --name 08-pools --section Pools --cols 110 --rows 28
cargo run --example capture -- --name 05-check-host --cmd 'sudo ./zfsctl/target/release/zfsctl check-host fbsd16'
```

Options: `--name` (required), `--section` (a sidebar name, reached with Tab), `--keys` (typed after that: `<Tab>`, `<S-Tab>`, `<Enter>`, `<Esc>`, `<Up>`, `<Down>`, `<Left>`, `<Right>`, `<Space>`, `<Backspace>`, `<F2>`, `<F5>`, `<Pause>` (waits 4 seconds, for a list on a remote host to fill), anything else as typed), `--cols`/`--rows` (default 110x30), `--wait` (seconds before the first key, default 10), `--after` (seconds after the last key, default 1.5), `--crop FIRST:LAST` (0-based, LAST exclusive), `--cmd` (capture a command's output instead of the TUI).

The hub is `HUB` (default `fb@172.16.10.90`) and the binary `ZFSCTL_BIN` (default `./zfsctl/target/release/zfsctl`, a build in the hub user's home). Email addresses in a capture become `you@example.com` and lines that mention a password are dropped. Names of real people in the test machines' data (`alice`, `carol`) become `alice` and `carol`, the same length, so columns stay aligned; `cargo run --example capture -- --rescrub` applies the same rule to the captures and the `screen` blocks already in the chapters.
