# Contributing

## Development Setup

After cloning, enable the pre-commit hooks (formatting + lint checks):

```sh
git config core.hooksPath .githooks
```

This runs `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings` before every commit.

## Running Tests

Standard (offline) tests:

```sh
cargo test
```

Elasticsearch integration tests require a live ES instance. Start one with docker compose:

```sh
make es-up      # start ES 9 container
make es-init    # wait until healthy, then provision index template

ES_TEST_URL=http://localhost:9292 \
ES_TEST_USER=elastic \
ES_TEST_PASSWORD=changeme \
cargo test --test elasticsearch_store_test -- --include-ignored

make es-down    # tear down
```

The ES tests are marked `#[ignore]` so they appear as `ignored` in normal `cargo test` output and are skipped unless explicitly included with `--include-ignored`.
