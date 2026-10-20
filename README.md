# spider-cloud-rs

Rust tools for:

- crawling CFMMC settlement data
- archiving local files
- uploading files or archives to cloud storage

The repository currently contains three binaries:

- `cfmmc-crawler-rs`: logs in to CFMMC, downloads daily settlement files, parses them, and sends notifications
- `cloud-uploader-rs`: uploads files or archives according to a TOML config
- `backup-to-cloud`: creates compressed backup archives and uploads them to configured cloud targets

## Requirements

- Rust stable
- network access during build/test
- local ONNX CAPTCHA model files for the CFMMC crawler, or Baidu OCR API credentials when using the Baidu fallback

## Project Layout

```text
src/bin/cfmmc-crawler-rs/   CFMMC crawler
src/bin/cloud-uploader-rs/  uploader entry
src/bin/backup-to-cloud.rs  backup entry
```

## Build

```bash
cargo build --release --bins
```

## Test

```bash
cargo test --all-targets
```

## Configuration

Example config files are included in the repository root:

- `cfmmc.toml.example`
- `cloud-uploader.toml.example`
- `backup.toml.example`

Typical usage is to copy one of these into your own local config file and pass the path as the first argument.

## Usage

### CFMMC crawler

```bash
cargo run --release --bin cfmmc-crawler-rs -- cfmmc.toml
```

Default config path is `config.toml` if no argument is provided.

The CFMMC crawler can use a local ONNX CAPTCHA recognizer or Baidu OCR's HTTP API. Configure the recognizer in your TOML file:

```toml
[captcha]
provider = "onnx" # onnx, baidu, onnx_then_baidu, baidu_then_onnx

[onnx_captcha]
model_path = "models/model.onnx"
vocab_path = "models/vocab.txt"
captcha_length = 6
```

If `provider` uses Baidu OCR, configure `baidu_ocr.api_key` and `baidu_ocr.secret_key`. The crawler fetches and caches `access_token` automatically during each run.

### Cloud uploader

```bash
cargo run --release --bin cloud-uploader-rs -- cloud-uploader.toml
```

### Backup to cloud

```bash
cargo run --release --bin backup-to-cloud -- backup.toml
```

## GitHub Actions

The repository includes GitHub Actions workflows under `.github/workflows`:

- `build.yml`: checks formatting, runs Clippy, builds and tests in Debian 13 containers and on Windows; verifies Linux binary startup in a clean Debian 13 container
- `release.yml`: builds release artifacts for tags and publishes archives

Linux release binaries target Debian 13 (trixie), x86_64. They are compiled in
the official `rust:1-trixie` container and checked for shared-library resolution
and startup in a clean `debian:13-slim` container before packaging. The Ubuntu
Actions runner only hosts Docker; it does not compile the Linux release binaries.
On Debian 13, install `ca-certificates`, `libgcc-s1`, and `libstdc++6`.
Archive mode in `cloud-uploader-rs` also requires `7z` in `PATH`.
Windows release binaries continue to be built on Windows.

The release workflow also packages:

- all three binaries
- example config files
- local ONNX CAPTCHA model files (`models/`)

Release notes are generated with `git-cliff` from `cliff.toml`.

## Notes

- `cfmmc-crawler-rs` requires valid account credentials and either local ONNX CAPTCHA model files or Baidu OCR credentials
- notification and cloud upload behavior depends entirely on the TOML config you provide
