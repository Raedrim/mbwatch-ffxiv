<p align="center">
  <img src="https://img.shields.io/badge/mbwatch-FFXIV%20Market%20Board-9b59b6?style=for-the-badge" alt="mbwatch">
</p>

<p align="center">
  <a href="https://github.com/Raedrim/mbwatch/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflow/status/Raedrim/mbwatch/release.yml?branch=main&logo=githubactions&logoColor=white&label=CI" alt="CI"></a>
  <a href="https://github.com/Raedrim/mbwatch/releases/latest"><img src="https://img.shields.io/github/v/release/Raedrim/mbwatch?logo=github&label=release" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/Raedrim/mbwatch?color=blue" alt="License: MIT"></a>
  <a href="https://aur.archlinux.org/packages/mbwatch"><img src="https://img.shields.io/aur/version/mbwatch?logo=archlinux&logoColor=white&label=AUR" alt="AUR"></a>
  <a href="https://github.com/Raedrim/mbwatch/pkgs/container/mbwatch"><img src="https://img.shields.io/badge/ghcr.io-image-2496ED?logo=docker&logoColor=white" alt="Container image"></a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/rust-2021-orange?logo=rust&logoColor=white" alt="Rust 2021">
  <img src="https://img.shields.io/badge/clippy-clean-success?logo=rust&logoColor=white" alt="clippy clean">
  <img src="https://img.shields.io/badge/code%20style-rustfmt-informational?logo=rust&logoColor=white" alt="rustfmt">
  <img src="https://img.shields.io/badge/methodology-TDD-brightgreen" alt="Test-driven">
  <img src="https://img.shields.io/badge/PRs-welcome-brightgreen" alt="PRs welcome">
  <img src="https://img.shields.io/github/last-commit/Raedrim/mbwatch" alt="Last commit">
  <img src="https://img.shields.io/github/repo-size/Raedrim/mbwatch" alt="Repo size">
</p>

# mbwatch

Headless FFXIV Market Board CLI and alert daemon, powered by the public [Universalis](https://universalis.app) v2 API.

```sh
mbwatch update-db                          # cache worlds, data centers, marketable item names
mbwatch check --config watchlist.yaml      # one-shot price comparison table
mbwatch watch --config watchlist.yaml      # daemon: Discord/Telegram alerts (--once for a single cycle)
```

See [`watchlist.example.yaml`](watchlist.example.yaml) for the config format. Alerts fire when the cheapest
price on your data center drops below `alert_below`, or when it undercuts your home world by `arbitrage_margin`.
Identical alerts (same item/quality/world/price) are suppressed for `cooldown` unless the price drops further.

Paths and endpoints are overridable for testing and containers: `--cache-dir`, `--state-file`,
`--universalis-url`, `--items-url` (env: `MBWATCH_CACHE_DIR`, `MBWATCH_STATE_FILE`, `MBWATCH_UNIVERSALIS_URL`, `MBWATCH_ITEMS_URL`).

## Install

```sh
# Arch Linux (AUR)
yay -S mbwatch          # or: paru -S mbwatch

# From source
cargo install --git https://github.com/Raedrim/mbwatch
```

Prebuilt binaries (Linux musl, Windows, macOS) are attached to each [release](https://github.com/Raedrim/mbwatch/releases).

## Docker

```sh
docker run --rm -v mbwatch-data:/data ghcr.io/raedrim/mbwatch update-db
docker run -d -v mbwatch-data:/data -v $PWD/watchlist.yaml:/watchlist.yaml:ro \
  -e DISCORD_WEBHOOK_URL ghcr.io/raedrim/mbwatch watch --config /watchlist.yaml
```

## Development

Built test-first: `cargo test` (unit + wiremock integration + `assert_cmd` end-to-end), `cargo clippy --all-targets -- -D warnings`.

## License

MIT — see [LICENSE](LICENSE).
