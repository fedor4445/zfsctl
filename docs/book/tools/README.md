# Capture tools

`capture.py` takes the screens and command outputs the book shows. They are real: the tool drives the real TUI (or runs the real command) on the test hub and writes the text to `../captures/NAME.txt`. A chapter embeds a capture unchanged in a fenced block tagged `screen`; the book's tests check that the embedded text equals a capture file, so a capture is never edited by hand.

Setup:

```sh
python3 -m venv /tmp/book-venv && /tmp/book-venv/bin/pip install pexpect pyte
```

Examples (from the repository root):

```sh
/tmp/book-venv/bin/python docs/book/tools/capture.py --name 08-pools --section Pools --cols 110 --rows 28
/tmp/book-venv/bin/python docs/book/tools/capture.py --name 05-check-host --cmd 'sudo ./zfsctl/target/release/zfsctl check-host fbsd16'
```

The hub is `HUB` (default `fb@172.16.10.90`) and the binary `ZFSCTL_BIN` (default `./zfsctl/target/release/zfsctl`, a build in the hub user's home). Email addresses in a capture become `you@example.com` and lines that mention a password are dropped.
