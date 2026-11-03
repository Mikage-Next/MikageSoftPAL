use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use pal_asset::{AssetSource, Nls, ResourceManager};

#[derive(Parser, Debug)]
#[command(author, version, about = "PAL asset loading validation tool")]
struct Cli {
    #[arg(value_name = "GAME_ROOT")]
    root: PathBuf,

    #[arg(long, value_enum, default_value_t = NlsArg::Sjis)]
    nls: NlsArg,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum NlsArg {
    Sjis,
    Gbk,
    Utf8,
}

impl From<NlsArg> for Nls {
    fn from(value: NlsArg) -> Self {
        match value {
            NlsArg::Sjis => Nls::ShiftJis,
            NlsArg::Gbk => Nls::Gbk,
            NlsArg::Utf8 => Nls::Utf8,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    Paths,
    ListPacs,
    Open {
        #[arg(value_name = "ASSET_NAME")]
        name: String,

        #[arg(short, long)]
        output: Option<PathBuf>,

        #[arg(long)]
        decrypt: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let nls = Nls::from(cli.nls);
    let mut manager = ResourceManager::bootstrap(&cli.root, nls)
        .with_context(|| format!("failed to bootstrap assets from {}", cli.root.display()))?;

    match cli.command {
        Command::Paths => {
            for path in manager.paths() {
                println!("{}", path);
            }
        }
        Command::ListPacs => {
            manager.preload_pacs()?;
            let mut pacs: Vec<_> = manager.list_loaded_pacs().collect();
            pacs.sort_by(|a, b| a.path().cmp(b.path()));
            for pac in pacs {
                println!("# {}", pac.path().display());
                for entry in pac.entries() {
                    println!(
                        "{}\t0x{:08X}\t0x{:08X}",
                        entry.display_name_lossy(nls),
                        entry.data_offset,
                        entry.data_size
                    );
                }
            }
        }
        Command::Open {
            name,
            output,
            decrypt,
        } => {
            let asset = if decrypt {
                manager.open_decrypting(&name)?
            } else {
                manager.open(&name)?
            };

            match &asset.source {
                AssetSource::Loose { path } => eprintln!("source=loose:{}", path.display()),
                AssetSource::Pac { pac_path, .. } => eprintln!("source=pac:{}", pac_path.display()),
                AssetSource::Patch { path } => eprintln!("source=patch:{}", path.display()),
            }
            eprintln!("size={}", asset.bytes.len());

            if let Some(output) = output {
                fs::write(&output, &asset.bytes)
                    .with_context(|| format!("failed to write {}", output.display()))?;
            } else {
                print_hex_preview(&asset.bytes);
            }
        }
    }
    Ok(())
}

fn print_hex_preview(bytes: &[u8]) {
    let shown = bytes.len().min(256);
    for (line, chunk) in bytes[..shown].chunks(16).enumerate() {
        print!("{:08X}  ", line * 16);
        for i in 0..16 {
            if let Some(b) = chunk.get(i) {
                print!("{:02X} ", b);
            } else {
                print!("   ");
            }
        }
        print!(" ");
        for b in chunk {
            let c = if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            };
            print!("{}", c);
        }
        println!();
    }
    if bytes.len() > shown {
        eprintln!("... truncated preview, total {} bytes", bytes.len());
    }
}
