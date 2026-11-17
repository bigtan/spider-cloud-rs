# spider-cloud-rs

A 6-crate Rust workspace for:

- crawling CFMMC settlement data (`cfmmc-crawler-rs`)
- archiving local files and uploading files or archives to cloud storage (`cloud-uploader-rs`)
- creating compressed backup archives, uploading them to cloud storage, and sending structured notifications (`backup-to-cloud`)

## Workspace Layout

```text
crates/
  spider-common/       Shared FS atomics, CLI parsing, logging, retry, templates, and archiving (multi-threaded .tar.zst)
  spider-notify/       Chanify, SMTP Email (HTML-escaped), and encrypted Pushgo dispatchers
  spider-uploader/     BaiduPan & Cloud189 uploaders, single-pass digests, and parallel UploadCoordinator
bins/
  cfmmc-crawler-rs/    CFMMC crawler & daily settlement parser (ONNX / Baidu OCR CAPTCHA)
  cloud-uploader-rs/   Archive/Files/Hybrid cloud uploader
  backup-to-cloud/     Scheduled command/archive backup runner with cloud upload & notifications
```

## Build & Test

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo build --release --bins --locked
```

## CLI Flags

All three binaries share a unified CLI parser (`spider_common::cli::parse_cli`):

```text
<binary> [OPTIONS] [CONFIG_PATH]
```

- `-h`, `--help`: print help and exit `0`
- `-V`, `--version`: print version and exit `0`
- `--date <YYYYMMDD|YYYY-MM-DD>` (or `--date=<...>`): override `custom_date` from config
- `--dry-run`: validate config, credentials, and paths without executing network calls, archiving, or notifications
- `--config <PATH>` or positional `<CONFIG_PATH>`: path to TOML config (default `config.toml`; passing more than one positional argument exits `1` with `"Too many arguments"`)

## Configuration

All three binaries use strict nested TOML schemas (`#[serde(deny_unknown_fields)]`):

- [`cfmmc.toml.example`](cfmmc.toml.example)
- [`cloud-uploader.toml.example`](cloud-uploader.toml.example)
- [`backup.toml.example`](backup.toml.example)

Copy an example file to a local config (ignored by git) and fill in your credentials, or supply credentials via environment variables (`CHANIFY_*`, `EMAIL_*`, `PUSHGO_*`, `BAIDU_APP_*`, `CLOUD189_*`, `BAIDU_OCR_*`).

### `cfmmc-crawler-rs`

```bash
cargo run --release --bin cfmmc-crawler-rs -- cfmmc.toml
```

- Uses `[captcha]`, `[onnx_captcha]`, `[baidu_ocr]`, `[account]`, and `[notifier.{chanify,email,pushgo}]`.
- Supports `custom_date`, `data_dir`, `log_dir`, and `log_retain_days`.

### `cloud-uploader-rs`

```bash
cargo run --release --bin cloud-uploader-rs -- cloud-uploader.toml
```

- Supports `upload_mode = "archive" | "files" | "hybrid"`.
- `[archive]` uses pure-Rust multi-threaded `.tar.zst` (`archive/{date}.tar.zst` or `.tzst`) with atomic writes and archive integrity verification.
- `[files]` uploads matching glob files (empty matches cleanly skip with `NoFilesFound`).
- `[uploader.{baidu,cloud189}]` and `[notifier.{chanify,email,pushgo}]`.

### `backup-to-cloud`

```bash
cargo run --release --bin backup-to-cloud -- backup.toml
```

- Uses `work_dir` (defaulting to the OS temporary directory), `compression_level`, `threads`, `[uploader.*]`, `[notifier.*]`, and `[[backups]]`.
- Validates `archive_name` against path traversal and leading `-` before execution.

## Security & Performance Architecture

- **Memory safety**: `#![forbid(unsafe_code)]` enforced workspace-wide via `[workspace.lints.rust]`.
- **Credential & path hardening**: redacted `Debug` on secret-bearing structs, private `0o600` atomic file writes (`AtomicTempFile` / `write_private_atomic`), HTTPS enforcement for notifier endpoints and Cloud189 upload URLs, HTML escaping for email bodies, URL-encoded Cloud189 filenames, and `validate_safe_name` guards.
- **Throughput**: single-pass streaming file digest computation, bounded `std::thread::scope` parallel chunk/part uploads, parallel multi-cloud upload via `UploadCoordinator`, Baidu rapid upload + partial `block_list` support, Cloud189 folder/session caching, multi-threaded `zstd`, and concurrent `NotificationDispatcher`.
- **Binary isolation**: `cloud-uploader-rs` and `backup-to-cloud` have zero dependency on `ort`, `calamine`, `image`, or `ndarray`, while `cfmmc-crawler-rs` has zero dependency on `spider-uploader`, `rsa`, or `zstd`.

## GitHub Actions

- `build.yml`: runs formatting checks, workspace Clippy (`--locked`), Debian 13 container builds/tests (`rust:1-trixie`), clean Debian 13 runtime verification (`debian:13-slim`), and Windows `--locked` builds/tests.
- `release.yml`: builds Debian 13 and Windows release archives packaged with example configs and ONNX model files.
