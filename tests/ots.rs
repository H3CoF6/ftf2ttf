//! 用本地 OTS 校验转换产物，避免每次都等浏览器报错。
//!
//! 需要先安装 `ots-sanitize`：
//!
//! ```bash
//! ./scripts/setup-ots.sh
//! ```
//!
//! 装好后直接 `cargo test --test ots` 即可。若找不到二进制，测试会跳过而不是失败，
//! 这样在没装 OTS 的环境里也能跑其它测试。

use ftf2ttf::{convert_ftf_with, ots, ConvertOptions};
use std::fs;
use std::path::PathBuf;

fn resource_fonts() -> Vec<PathBuf> {
    let mut fonts = Vec::new();
    let Ok(entries) = fs::read_dir("resources") else {
        return fonts;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if let Ok(files) = fs::read_dir(&dir) {
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttf")) {
                    fonts.push(path);
                }
            }
        }
    }
    fonts.sort();
    fonts
}

#[test]
fn converted_fonts_pass_ots() {
    if ots::find_ots_binary().is_none() {
        eprintln!(
            "skipping OTS validation: ots-sanitize not found; run scripts/setup-ots.sh \
             or set OTS_SANITIZE"
        );
        return;
    }

    let fonts = resource_fonts();
    assert!(!fonts.is_empty(), "no resource fonts found under resources/");

    let out_dir = PathBuf::from("tmp/ots_test");
    fs::create_dir_all(&out_dir).expect("create tmp dir");

    let mut checked = 0;
    for font in &fonts {
        let raw = fs::read(font).expect("read resource font");
        let stem = font
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "font".to_string());

        for color in [false, true] {
            let opts = ConvertOptions {
                color,
                ..Default::default()
            };
            let converted = convert_ftf_with(&raw, &opts)
                .unwrap_or_else(|e| panic!("convert {} failed: {e}", font.display()));
            let suffix = if color { "color" } else { "plain" };
            let path = out_dir.join(format!("{stem}-{suffix}.ttf"));
            fs::write(&path, &converted).expect("write converted font");

            let report = ots::sanitize_file(&path).expect("run ots");
            assert!(
                report.success,
                "OTS rejected {}:\n{}",
                path.display(),
                report.raw_stderr.trim()
            );
            checked += 1;
        }
    }
    eprintln!("OTS validated {checked} converted font(s)");
}
