## What and why

<!-- What does this change, and why? Link the issue it fixes, e.g. "Fixes #12". -->

## How it was tested

<!-- Windows version, Sunshine version, and what you checked (with the phone, if it touches pairing or the library). -->

## Checklist

- [ ] `cargo clippy --workspace --all-targets` and `cargo test --workspace` pass
- [ ] Matches the phone app's words, look and sounds
- [ ] Memory checked with the software renderer (window open and in the tray), if the UI changed
- [ ] Changes to the QR pairing contract are in `docs/pair-with-scan.md` and agreed with the phone app
- [ ] New dependencies are GPLv3-compatible (and listed in `NOTICE` if notable)
