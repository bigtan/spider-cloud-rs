use crate::Result;

/// Parsed command-line options common to all spider-cloud binaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub config_path: String,
    pub date_override: Option<String>,
    pub dry_run: bool,
}

/// Result of parsing CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    Run(CliArgs),
    PrintHelp(String),
    PrintVersion(String),
}

/// Parse CLI arguments (excluding `argv[0]`), supporting `-h`/`--help`, `-V`/`--version`,
/// `--dry-run`, `--date <val>`/`--date=<val>`, `--config <path>`/`--config=<path>`,
/// and at most one positional `<config.toml>` (defaulting to `"config.toml"`).
pub fn parse_cli<I, S>(bin_name: &str, version: &str, about: &str, args: I) -> Result<CliAction>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter().map(|s| s.as_ref().to_string());
    let mut config_path: Option<String> = None;
    let mut date_override: Option<String> = None;
    let mut dry_run = false;
    let mut end_of_flags = false;

    while let Some(arg) = iter.next() {
        if !end_of_flags {
            match arg.as_str() {
                "-h" | "--help" => {
                    return Ok(CliAction::PrintHelp(format_help(bin_name, version, about)));
                }
                "-V" | "--version" => {
                    return Ok(CliAction::PrintVersion(format!("{bin_name} {version}")));
                }
                "--dry-run" => {
                    dry_run = true;
                    continue;
                }
                "--date" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--date requires a value"))?;
                    if val.trim().is_empty() {
                        anyhow::bail!("--date requires a non-empty value");
                    }
                    date_override = Some(val);
                    continue;
                }
                "--config" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--config requires a path"))?;
                    if val.trim().is_empty() {
                        anyhow::bail!("--config requires a non-empty path");
                    }
                    if config_path.is_some() {
                        anyhow::bail!("Too many arguments");
                    }
                    config_path = Some(val);
                    continue;
                }
                "--" => {
                    end_of_flags = true;
                    continue;
                }
                _ => {
                    if let Some(val) = arg.strip_prefix("--date=") {
                        if val.trim().is_empty() {
                            anyhow::bail!("--date requires a non-empty value");
                        }
                        date_override = Some(val.to_string());
                        continue;
                    }
                    if let Some(val) = arg.strip_prefix("--config=") {
                        if val.trim().is_empty() {
                            anyhow::bail!("--config requires a non-empty path");
                        }
                        if config_path.is_some() {
                            anyhow::bail!("Too many arguments");
                        }
                        config_path = Some(val.to_string());
                        continue;
                    }
                    if arg.starts_with('-') && arg != "-" {
                        anyhow::bail!("Unknown argument: {arg}");
                    }
                }
            }
        }

        if config_path.is_some() {
            anyhow::bail!("Too many arguments");
        }
        config_path = Some(arg);
    }

    Ok(CliAction::Run(CliArgs {
        config_path: config_path.unwrap_or_else(|| "config.toml".to_string()),
        date_override,
        dry_run,
    }))
}

fn format_help(bin_name: &str, version: &str, about: &str) -> String {
    format!(
        "{bin_name} {version}\n{about}\n\n\
         Usage: {bin_name} [OPTIONS] [CONFIG]\n\n\
         Arguments:\n  \
           [CONFIG]              Path to TOML configuration file [default: config.toml]\n\n\
         Options:\n  \
               --config <PATH>   Path to TOML configuration file\n  \
               --date <DATE>     Override target date (YYYYMMDD or YYYY-MM-DD)\n  \
               --dry-run         Validate configuration and stage plan without remote I/O\n  \
           -h, --help            Print help\n  \
           -V, --version         Print version\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_config_toml() {
        let action = parse_cli("tool", "0.2.0", "desc", Vec::<&str>::new()).unwrap();
        assert_eq!(
            action,
            CliAction::Run(CliArgs {
                config_path: "config.toml".to_string(),
                date_override: None,
                dry_run: false,
            })
        );
    }

    #[test]
    fn parses_flags_and_positional_config() {
        let action = parse_cli(
            "tool",
            "0.2.0",
            "desc",
            ["--dry-run", "--date", "2026-06-11", "custom.toml"],
        )
        .unwrap();
        assert_eq!(
            action,
            CliAction::Run(CliArgs {
                config_path: "custom.toml".to_string(),
                date_override: Some("2026-06-11".to_string()),
                dry_run: true,
            })
        );

        let action_eq = parse_cli(
            "tool",
            "0.2.0",
            "desc",
            ["--config=alt.toml", "--date=20260611"],
        )
        .unwrap();
        assert_eq!(
            action_eq,
            CliAction::Run(CliArgs {
                config_path: "alt.toml".to_string(),
                date_override: Some("20260611".to_string()),
                dry_run: false,
            })
        );
    }

    #[test]
    fn handles_help_and_version() {
        let help = parse_cli("cfmmc", "0.2.0", "Crawler", ["-h"]).unwrap();
        assert!(matches!(help, CliAction::PrintHelp(text) if text.contains("cfmmc 0.2.0")));

        let ver = parse_cli("cfmmc", "0.2.0", "Crawler", ["--version"]).unwrap();
        assert_eq!(ver, CliAction::PrintVersion("cfmmc 0.2.0".to_string()));
    }

    #[test]
    fn rejects_too_many_arguments_and_unknown_flags() {
        let err = parse_cli("tool", "0.2.0", "desc", ["unused.toml", "extra"]).unwrap_err();
        assert!(err.to_string().contains("Too many arguments"));

        assert!(parse_cli("tool", "0.2.0", "desc", ["--bogus"]).is_err());
        assert!(parse_cli("tool", "0.2.0", "desc", ["--date"]).is_err());
        assert!(parse_cli("tool", "0.2.0", "desc", ["--config"]).is_err());
    }
}
