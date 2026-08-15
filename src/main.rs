use anyhow::{bail, Context, Result};
use clap::Parser;
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(name = "ftf2ttf", about = "Convert Tencent FTF modified TTF to standard TTF")]
struct Cli {
    /// Input FTF font file or directory
    #[arg(required = true)]
    input: PathBuf,

    /// Output TTF font file or directory
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Batch convert recursively if input is a directory
    #[arg(short, long)]
    recursive: bool,
}

fn is_normal_ttf(raw: &[u8]) -> bool {
    if raw.len() < 12 {
        return false;
    }
    let num_tables = u16::from_be_bytes([raw[4], raw[5]]) as usize;
    if raw.len() < 12 + num_tables * 16 {
        return false;
    }

    let mut has_ftfh = false;
    let mut has_ftfg = false;

    for i in 0..num_tables {
        let off = 12 + i * 16;
        if off + 4 > raw.len() {
            break;
        }
        let tag = &raw[off..off + 4];
        if tag == b"FTFH" {
            has_ftfh = true;
        }
        if tag == b"FTFG" {
            has_ftfg = true;
        }
    }

    !has_ftfh && !has_ftfg
}

fn process_single_file(src: &Path, dst: &Path) -> Result<()> {
    let raw = fs::read(src).with_context(|| format!("Failed to read {:?}", src))?;

    if is_normal_ttf(&raw) {
        println!("Skipping {:?}: already a normal TTF", src.file_name().unwrap());
        return Ok(());
    }

    let converted = ftf2ttf::convert_ftf(&raw).with_context(|| format!("Failed to convert {:?}", src))?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dst, converted).with_context(|| format!("Failed to write {:?}", dst))?;
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let start_time = Instant::now();

    if cli.input.is_file() {
        let dst = cli.output.unwrap_or_else(|| {
            let stem = cli.input.file_stem().unwrap_or_default().to_string_lossy();
            let parent = cli.input.parent().unwrap_or_else(|| Path::new("."));
            parent.join(format!("{}_result.ttf", stem))
        });
        println!("Converting {:?} -> {:?}", cli.input, dst);
        process_single_file(&cli.input, &dst)?;
        println!("Done in {:.2}ms", start_time.elapsed().as_secs_f64() * 1000.0);
    } else if cli.input.is_dir() {
        let out_dir = cli.output.unwrap_or_else(|| cli.input.join("converted"));
        let mut tasks = Vec::new();

        let walker = if cli.recursive {
            WalkDir::new(&cli.input)
        } else {
            WalkDir::new(&cli.input).max_depth(1)
        };

        for entry in walker.into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension() {
                    let ext = ext.to_string_lossy().to_lowercase();
                    if ext == "ttf" || ext == "ftf" {
                        let rel = path.strip_prefix(&cli.input)?;
                        let mut target = out_dir.join(rel);
                        target.set_extension("ttf");
                        tasks.push((path.to_path_buf(), target));
                    }
                }
        }

        println!("Found {} files to convert...", tasks.len());
        tasks.par_iter().for_each(|(src, dst)| {
            match process_single_file(src, dst) {
                Ok(_) => println!("Converted: {:?}", src.file_name().unwrap()),
                Err(e) => eprintln!("Error converting {:?}: {:#}", src, e),
            }
        });

        println!(
            "Batch conversion finished in {:.2}s",
            start_time.elapsed().as_secs_f64()
        );
    } else {
        bail!("Input path does not exist");
    }

    Ok(())
}
