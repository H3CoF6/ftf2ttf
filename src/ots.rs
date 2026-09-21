//! 本地 OTS（OpenType Sanitizer）校验支持。
//!
//! 浏览器（Chromium/Electron）加载字体前会先过 OTS，报错时只会丢一句
//! "Failed to decode downloaded font"，很难定位。为了不必每次都等浏览器，
//! 这里直接调用 OTS 官方的 `ots-sanitize` 命令行工具做本地预检。
//!
//! 获取方式见 `scripts/setup-ots.sh`（装到项目内 `.venv-ots`，不污染全局）。

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// OTS 一次校验的结果。
#[derive(Debug, Clone)]
pub struct OtsReport {
    /// OTS 是否通过（退出码为 0）。
    pub success: bool,
    /// 错误行（stderr 中以 `ERROR:` 开头的行）。
    pub errors: Vec<String>,
    /// 警告行（stderr 中以 `WARNING:` 开头的行）。
    pub warnings: Vec<String>,
    /// OTS 的完整 stderr，便于原样打印。
    pub raw_stderr: String,
}

/// 定位 `ots-sanitize` 可执行文件。
///
/// 查找顺序：
/// 1. 环境变量 `OTS_SANITIZE` 指定的路径；
/// 2. `PATH` 中的 `ots-sanitize`；
/// 3. 项目内 `.venv-ots`（由 `scripts/setup-ots.sh` 创建）里
///    `opentype-sanitizer` 自带的二进制。
pub fn find_ots_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OTS_SANITIZE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = which("ots-sanitize") {
        return Some(p);
    }
    for root in [".venv-ots", "../.venv-ots"] {
        if let Some(p) = bundled_ots(Path::new(root)) {
            return Some(p);
        }
    }
    None
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

fn bundled_ots(venv: &Path) -> Option<PathBuf> {
    let lib = venv.join("lib");
    let entries = std::fs::read_dir(lib).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("site-packages/ots/ots-sanitize");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 用 OTS 校验一个字体文件。
///
/// `ots-sanitize` 的用法是 `ots-sanitize <input> [dest]`；这里把结果写到临时文件，
/// 只关心它是否成功、以及 stderr 里的诊断信息。
pub fn sanitize_file(font: &Path) -> Result<OtsReport> {
    let bin = find_ots_binary().ok_or_else(|| {
        anyhow::anyhow!(
            "ots-sanitize not found; run scripts/setup-ots.sh or set OTS_SANITIZE"
        )
    })?;

    let dest = std::env::temp_dir().join(format!(
        "ftf2ttf-ots-{}.out.ttf",
        std::process::id()
    ));
    let output = Command::new(&bin)
        .arg(font)
        .arg(&dest)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run {}: {e}", bin.display()))?;
    let _ = std::fs::remove_file(&dest);

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let is_diag = |prefix: &str| {
        stderr
            .lines()
            .filter(|l| l.starts_with(prefix))
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
    };

    Ok(OtsReport {
        success: output.status.success(),
        errors: is_diag("ERROR:"),
        warnings: is_diag("WARNING:"),
        raw_stderr: stderr,
    })
}

/// 校验字体并在失败时返回带诊断信息的错误，供 CLI 直接使用。
pub fn assert_sanitized(font: &Path) -> Result<OtsReport> {
    let report = sanitize_file(font)?;
    if !report.success {
        bail!(
            "OTS rejected {}:\n{}",
            font.display(),
            report.raw_stderr.trim()
        );
    }
    Ok(report)
}
