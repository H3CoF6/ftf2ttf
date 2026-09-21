use anyhow::{bail, Context, Result};
use clap::Parser;
use ftf2ttf::{convert_ftf_with, extract_eimg_frames, ConvertOptions};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(
    name = "ftf2ttf",
    about = "Convert Tencent FTF modified TTF to standard TTF"
)]
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

    /// Produce a colored font (COLR v1 + CPAL v0) from the private brsh/cglf tables
    #[arg(short = 'c', long)]
    color: bool,

    /// Extra characters to force-color (the QQ theme's per-character list is not in the FTF)
    #[arg(long, value_name = "CHARS")]
    color_chars: Option<String>,

    /// Export embedded eimg PNG frames (color/animation fonts) into this directory
    #[arg(long, value_name = "DIR")]
    dump_assets: Option<PathBuf>,

    /// Validate every produced font with the local ots-sanitize (see scripts/setup-ots.sh)
    #[arg(long)]
    check_ots: bool,
}

fn dump_eimg(raw: &[u8], dir: &Path) -> Result<usize> {
    let frames = extract_eimg_frames(raw)?;
    if frames.is_empty() {
        return Ok(0);
    }
    fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create asset directory {:?}", dir))?;
    for frame in &frames {
        let name = format!("frame_{:03}.png", frame.slot);
        fs::write(dir.join(name), &frame.png)
            .with_context(|| format!("Failed to write frame {:?}", dir))?;
    }
    Ok(frames.len())
}

fn run_ots(dst: &Path) -> Result<()> {
    match ftf2ttf::ots::assert_sanitized(dst) {
        Ok(report) => {
            for warning in &report.warnings {
                eprintln!("  OTS {warning}");
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn process_single_file(
    src: &Path,
    dst: &Path,
    cli: &Cli,
    assets_root: Option<&Path>,
) -> Result<()> {
    let raw = fs::read(src).with_context(|| format!("Failed to read {:?}", src))?;

    if let Some(root) = assets_root {
        let stem = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "font".to_string());
        let count = dump_eimg(&raw, &root.join(&stem))?;
        if count > 0 {
            println!("Exported {count} image(s) for {stem}");
        }
    }

    let opts = ConvertOptions {
        color: cli.color,
        color_chars: cli.color_chars.as_ref().map(|s| s.chars().collect()),
    };
    let converted =
        convert_ftf_with(&raw, &opts).with_context(|| format!("Failed to convert {:?}", src))?;

    // 如果转换后的数据与原始数据相同，说明不需要修复
    if converted == raw {
        println!("Skipping {:?}: no issues found", src.file_name().unwrap());
        return Ok(());
    }

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dst, converted).with_context(|| format!("Failed to write {:?}", dst))?;

    if cli.check_ots {
        run_ots(dst).with_context(|| format!("OTS check failed for {:?}", dst))?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let start_time = Instant::now();

    if cli.input.is_file() {
        let dst = cli.output.clone().unwrap_or_else(|| {
            let stem = cli.input.file_stem().unwrap_or_default().to_string_lossy();
            let parent = cli.input.parent().unwrap_or_else(|| Path::new("."));
            parent.join(format!("{}_result.ttf", stem))
        });
        println!("Converting {:?} -> {:?}", cli.input, dst);
        process_single_file(&cli.input, &dst, &cli, cli.dump_assets.as_deref())?;
        println!("Done in {:.2}ms", start_time.elapsed().as_secs_f64() * 1000.0);
    } else if cli.input.is_dir() {
        let out_dir = cli
            .output
            .clone()
            .unwrap_or_else(|| cli.input.join("converted"));
        let mut tasks = Vec::new();

        let walker = if cli.recursive {
            WalkDir::new(&cli.input)
        } else {
            WalkDir::new(&cli.input).max_depth(1)
        };

        for entry in walker.into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension()
            {
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
            match process_single_file(src, dst, &cli, cli.dump_assets.as_deref()) {
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
