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
