# Contributing to Glimt

Bug reports, ideas and fixes are very welcome.

## Reporting a bug or asking for a feature

Open an [issue](https://github.com/thornbux/glimt/issues/new/choose). For a bug, say what you
did, what happened and what you expected, which version of Glimt (Help → About Glimt) and
which distribution and desktop you use, Wayland or X11.

## Changing the code

[AGENTS.md](AGENTS.md) describes how Glimt is built and what every change must respect: speed in
folders of any size, little memory, Geeqie's behaviour and keys, and the simplest solution that
works. It is written for coding agents, and holds for people just the same.

In short:

1. Fork the repository and make a branch.
2. Keep to one feature or fix per commit and per pull request.
3. Before committing, run:

   ```sh
   cargo fmt
   cargo clippy --release -- -D warnings
   cargo test --release
   cargo clippy --release --all-features -- -D warnings
   cargo test --release --all-features
   ```

4. Open a pull request. CI runs the same checks.

Code, comments, commit messages and the user interface are in English.

By contributing you agree that your contribution is licensed under the MIT license, as Glimt is.
