# The zfsctl site

A plain, text-first site in a terminal look: `index.html` and `style.css` by hand (no scripts, no web fonts, no trackers),
and the book built from `docs/book` with [mdBook](https://rust-lang.github.io/mdBook/) (`cargo install mdbook --locked`),
dressed by `theme/terminal.css`.

Build into `site/public/` (not tracked):

```sh
mdbook build site
cp site/index.html site/style.css site/public/
```

`site/public/` is the whole site: upload it as it is (Cloudflare Pages: Direct Upload, or connect the repository with the
build command above and `site/public` as the output directory). `docs/book/SUMMARY.md` is mdBook's table of contents; a test
keeps it equal to the chapter list in `docs/book/README.md`.
