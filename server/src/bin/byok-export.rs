//! Exports models and ports from a previous SQLite database into `config.toml`.
use std::path::PathBuf;

use cursor_server::{config, error::Result};

const USAGE: &str =
    "usage: byok-export --from <database> --out <config.toml> [--default-model <name>]";

struct Arguments {
    from: PathBuf,
    out: PathBuf,
    default_model: Vec<String>,
}

impl Arguments {
    fn parse(mut args: impl Iterator<Item = String>) -> std::result::Result<Self, String> {
        let mut from = None;
        let mut out = None;
        let mut default_model = Vec::new();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--from" => from = Some(args.next().ok_or("--from needs a path")?),
                "--out" => out = Some(args.next().ok_or("--out needs a path")?),
                "--default-model" => {
                    default_model.push(args.next().ok_or("--default-model needs a value")?)
                }
                "--help" | "-h" => return Err(USAGE.into()),
                other => return Err(format!("unexpected argument: {other}\n{USAGE}")),
            }
        }
        Ok(Self {
            from: PathBuf::from(from.ok_or("missing --from")?),
            out: PathBuf::from(out.ok_or("missing --out")?),
            default_model,
        })
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cursor_server=info".into()),
        )
        .init();
    let arguments = match Arguments::parse(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let summary = config::export(&arguments.from, &arguments.out, &arguments.default_model)?;
    tracing::info!(
        models = summary.model_count,
        providers = summary.provider_count,
        default_model = %summary.default_model,
        out = %arguments.out.display(),
        "exported configuration"
    );
    println!(
        "exported {} models from {} providers into {}",
        summary.model_count,
        summary.provider_count,
        arguments.out.display()
    );
    Ok(())
}
