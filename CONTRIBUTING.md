# Contributing

Contributions are welcome.

## Development

FnType currently targets Apple Silicon Macs running macOS 14 or later.

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Use `./scripts/bundle.sh` to install a local app bundle. Never commit API keys, private dictionary contents, signing certificates, or generated app bundles.

## Pull requests

- Keep changes focused and explain the user-facing behavior.
- Add tests for transcript, dictionary, and safety-policy changes.
- Run all checks above before opening the pull request.
- Preserve password-field, terminal, clipboard-restoration, and empty/error safeguards.

## Bug reports

Include the macOS version, Mac architecture, reproduction steps, and sanitized logs. Do not include transcripts or API keys.
