# Contributing to PocketDeck for PC

Thanks for helping out! Bug reports, ideas and pull requests are all welcome.

## Reporting bugs and asking for features

- Search [existing issues](https://github.com/MohammedAbidNafi/Pocket-Deck-PC/issues) first.
- Use the **Bug report** or **Feature request** template, and include your Windows version, PocketDeck version (Settings → About) and Sunshine version.
- Problems in the phone app belong in [PocketDeck](https://github.com/MohammedAbidNafi/Pocket-Deck/issues).
- Logs live in `%LOCALAPPDATA%\PocketDeck\logs`; Sunshine's own log is `sunshine.log` in its `config` folder.
- Security issues: don't open a public issue. See [SECURITY.md](SECURITY.md).

## Development setup

1. Install [Rust](https://rustup.rs) on Windows. [`rust-toolchain.toml`](rust-toolchain.toml) pins the version, and rustup fetches it on the first build.
2. Clone the repo, then build and test:

```bash
cargo build
```

```bash
cargo test --workspace
```

Set `POCKETDECK_HOME` to a scratch folder to keep your test settings away from your real ones.

## Making changes

- Branch from `main` and open a pull request; direct pushes to `main` are blocked.
- Keep PRs focused on one change. CI must pass (build, Clippy and tests on Windows) before merging.
- PRs are squash-merged, so the PR title becomes the commit message: write it as a short summary (e.g. `fix: tray icon after Explorer restarts`).
- Put logic in `crates/` (no UI there) and add unit tests for it. The `app/` crate is the window, tray and wiring.
- Match the phone app: same words, colours, motion and sounds. Port from [PocketDeck](https://github.com/MohammedAbidNafi/Pocket-Deck)'s source rather than inventing new styles.
- Keep it light. The software renderer is the default; check memory use after UI changes, both with the window open and in the tray.
- Slint 1.16 with the software renderer has a few traps: transforms, opacity and clipping are dropped on a component's root, and gradients lose rounded corners. Look at how existing components handle them.
- Changes to the QR pairing contract go in [`docs/pair-with-scan.md`](docs/pair-with-scan.md) first, and need a matching change in the phone app.

### Releasing

Releases are automatic. To ship a new version, bump `version` under `[workspace.package]` in [`Cargo.toml`](Cargo.toml) in a PR, and add a `### <version>` entry under **Release notes** in the [README](README.md). That entry becomes the top of the GitHub Release. When it merges into `main`, the **Release** workflow sees there's no `v<version>` tag yet, builds `PocketDeck.exe`, and publishes a GitHub Release with that tag, the program and generated notes. Pushes that don't change the version do nothing.

## Licence

PocketDeck for PC is licensed under the [GNU GPL v3](LICENSE). By contributing, you agree that your contributions are licensed under the same terms. New dependencies must be GPLv3-compatible, and notable ones listed in [`NOTICE`](NOTICE).

## Code of conduct

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
