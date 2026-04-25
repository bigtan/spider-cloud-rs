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
- Baidu OCR API credentials for the CFMMC crawler CAPTCHA recognizer

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

The CFMMC crawler uses Baidu OCR's HTTP API for CAPTCHA recognition. Configure `baidu_ocr.api_key` and `baidu_ocr.secret_key` in your TOML file. The crawler fetches and caches `access_token` automatically during each run.

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

- `build.yml`: builds and tests on Linux and Windows
- `release.yml`: builds release artifacts for tags and publishes archives

The release workflow also packages:

- all three binaries
- example config files

Release notes are generated with `git-cliff` from `cliff.toml`.

## Notes

- `cfmmc-crawler-rs` requires valid account credentials and Baidu OCR credentials
- notification and cloud upload behavior depends entirely on the TOML config you provide
