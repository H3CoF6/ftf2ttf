//! 导出 QQ「炫彩/场景字体」私有表 `eimg` 中内嵌的 PNG 帧序列。
//!
//! 这里只做「把图片拿出来」，不做动画编排逆向。`eimg` 的表结构（实测 20268 /
//! 20338 / 20405）：
//!
//! ```text
//! offset 0            u32  version = 0x00010000（4 字节之后是 u16 count + u16 0）
//! offset 4            u16  count         帧槽位数（20268 = 125、20338 = 136、20405 = 110）
//! offset 6            ...                保留字段，到 offset 16 为止
//! offset 16           u32[count + 1]     相对偏移表（相对下面的 base）
//! table_end           32 字节            编排记录，**基准 base 在它之后**（见下）
//! table_end + 32      数据区              每槽一块，通常以 PNG 签名开头
//! ```
//!
//! `base` 不能靠「首个 PNG 签名 - offsets[0]」反推：20405 的数据区**第 0 槽是一段
//! 32 字节的编排记录**（和 table_end 处那条同形状），首个 PNG 落在第 1 槽，于是
//! 反推出来的 base 高了 32 字节、整张表错位，一帧都取不到（实测修复前 0 帧、
//! 修复后 109 帧）。所以 base 优先按表几何算（`table_end + 32`），并用「偏移表里
//! 有多少个值正好落在 PNG 签名上」跟旧的签名反推法比一比，取更准的那个。
//!
//! 注意：槽位数可能大于实际图片数 —— 20405 的 count 是 110、真图只有 109 张。
//! 这里只导出真正以 PNG 签名开头、且能完整走到 `IEND` 的块，并保留其槽位序号。

use anyhow::{bail, Result};

/// PNG 文件签名。
const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// 偏移表与数据区之间那 32 字节编排记录的长度（三个实测样本都是 32）。
const INDEX_TAIL_LEN: usize = 32;

/// `eimg` 中的一帧图片。
#[derive(Debug, Clone)]
pub struct EimgFrame {
    /// 在 `eimg` 偏移表中的槽位序号（同一字体的多帧动画按此排序）。
    pub slot: usize,
    /// 图片宽度（像素），取自 IHDR。
    pub width: u32,
    /// 图片高度（像素），取自 IHDR。
    pub height: u32,
    /// 完整 PNG 字节（含签名与 `IEND`）。
    pub png: Vec<u8>,
}

/// 从原始字体数据中提取 `eimg` 内嵌的 PNG 帧序列。
///
/// - 若字体没有 `eimg` 表（普通字体或纯渐变彩色字体），返回空 `Vec`。
/// - 若 `eimg` 结构异常但找不到任何 PNG，同样返回空 `Vec`，不报错。
pub fn extract_eimg_frames(raw: &[u8]) -> Result<Vec<EimgFrame>> {
    let Some(eimg) = find_table(raw, b"eimg")? else {
        return Ok(Vec::new());
    };
    extract_frames_from_eimg(eimg)
}

/// 按标签查找原始 SFNT 容器中的表数据切片。
pub(crate) fn find_table<'a>(raw: &'a [u8], tag: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    if raw.len() < 12 {
        bail!("File too short to be a valid TTF");
    }
    let num_tables = u16::from_be_bytes([raw[4], raw[5]]) as usize;
    if raw.len() < 12 + num_tables * 16 {
        bail!("Corrupted table directory");
    }
    for i in 0..num_tables {
        let off = 12 + i * 16;
        if &raw[off..off + 4] != tag {
            continue;
        }
        let toff = u32::from_be_bytes([
            raw[off + 8],
            raw[off + 9],
            raw[off + 10],
            raw[off + 11],
        ]) as usize;
        let tlen = u32::from_be_bytes([
            raw[off + 12],
            raw[off + 13],
            raw[off + 14],
            raw[off + 15],
        ]) as usize;
        if toff + tlen > raw.len() {
            bail!("Table {:?} points outside file boundary", tag);
        }
        return Ok(Some(&raw[toff..toff + tlen]));
    }
    Ok(None)
}

fn extract_frames_from_eimg(eimg: &[u8]) -> Result<Vec<EimgFrame>> {
    if eimg.len() < 24 {
        return Ok(Vec::new());
    }
    let count = u16::from_be_bytes([eimg[4], eimg[5]]) as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    let idx_start = 16;
    let idx_end = idx_start + 4 * (count + 1);
    if eimg.len() < idx_end {
        return Ok(Vec::new());
    }

    let offsets: Vec<u64> = (0..=count)
        .map(|k| {
            let o = idx_start + 4 * k;
            u32::from_be_bytes([eimg[o], eimg[o + 1], eimg[o + 2], eimg[o + 3]]) as u64
        })
        .collect();

    let Some(base) = data_base(eimg, idx_end, &offsets) else {
        return Ok(Vec::new());
    };

    let mut frames = Vec::new();
    for slot in 0..count {
        let start = offsets[slot] as i64 + base;
        let end = offsets[slot + 1] as i64 + base;
        if start < 0 || end <= start {
            continue;
        }
        let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
            continue;
        };
        if start >= eimg.len() {
            continue;
        }
        let end = end.min(eimg.len());
        let blob = &eimg[start..end];
        if let Some(len) = png_total_len(blob) {
            let (width, height) = png_dimensions(blob);
            frames.push(EimgFrame {
                slot,
                width,
                height,
                png: blob[..len].to_vec(),
            });
        }
    }
    Ok(frames)
}

/// 数据区的基准：偏移表里每个值都是相对它的（见文件头）。
///
/// 两个候选各按「偏移表里有多少个值正好指向 PNG 签名」打分，取高的那个；同分时
/// 优先表几何法 —— 20405 那种「第 0 槽不是图片」的字体只有它能全中，而 20268 /
/// 20338 两种算法结果一致，不会因此改变行为。
fn data_base(eimg: &[u8], table_end: usize, offsets: &[u64]) -> Option<i64> {
    let hits = |base: i64| -> Option<usize> {
        if base < 0 {
            return None;
        }
        Some(
            offsets
                .iter()
                .filter(|&&off| matches_png_sig_at(eimg, base + off as i64))
                .count(),
        )
    };

    let geometry = i64::try_from(table_end + INDEX_TAIL_LEN)
        .ok()
        .and_then(|base| hits(base).map(|score| (score, base)));
    let legacy = find_png_signature(eimg, table_end)
        .map(|sig| sig as i64 - offsets[0] as i64)
        .and_then(|base| hits(base).map(|score| (score, base)));

    match (geometry, legacy) {
        (Some((geo, geo_base)), Some((old, old_base))) => {
            Some(if old > geo { old_base } else { geo_base })
        }
        (Some((_, base)), None) | (None, Some((_, base))) => Some(base),
        (None, None) => None,
    }
}

/// `at` 处是不是一张图的 PNG 签名。
fn matches_png_sig_at(data: &[u8], at: i64) -> bool {
    let Some(start) = usize::try_from(at).ok() else {
        return false;
    };
    match start.checked_add(PNG_SIG.len()) {
        Some(end) if end <= data.len() => data[start..end] == PNG_SIG,
        _ => false,
    }
}

fn find_png_signature(data: &[u8], from: usize) -> Option<usize> {
    if data.len() < 8 {
        return None;
    }
    data[from.min(data.len())..]
        .windows(8)
        .position(|w| w == PNG_SIG)
        .map(|p| p + from.min(data.len()))
}

/// 按 PNG 块结构走一遍，返回包含 `IEND` 在内的完整长度。
fn png_total_len(data: &[u8]) -> Option<usize> {
    if data.len() < 8 || data[..8] != PNG_SIG {
        return None;
    }
    let mut pos = 8usize;
    loop {
        if pos + 8 > data.len() {
            return None;
        }
        let len = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
            as usize;
        let ctype = &data[pos + 4..pos + 8];
        let next = pos.checked_add(12 + len)?;
        if next > data.len() {
            return None;
        }
        pos = next;
        if ctype == b"IEND" {
            return Some(pos);
        }
    }
}

/// 读取 IHDR 中的宽高（缺失时回退为 0）。
fn png_dimensions(data: &[u8]) -> (u32, u32) {
    if data.len() < 24 || data[12..16] != *b"IHDR" {
        return (0, 0);
    }
    let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    (w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1x1 RGBA PNG，用于构造最小可用的 `eimg` 表。
    const TINY_PNG: [u8; 70] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
        0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
        0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99,
        0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// 把一个 `eimg` 表包进最小的单表 SFNT 容器，供各用例复用。
    fn wrap_eimg(eimg: &[u8]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        raw.extend_from_slice(&1u16.to_be_bytes()); // numTables
        raw.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        raw.extend_from_slice(b"eimg");
        raw.extend_from_slice(&0u32.to_be_bytes()); // checksum
        raw.extend_from_slice(&(28u32).to_be_bytes()); // offset
        raw.extend_from_slice(&(eimg.len() as u32).to_be_bytes());
        raw.extend_from_slice(eimg);
        raw
    }

    #[test]
    fn png_len_rejects_garbage() {
        assert!(png_total_len(b"not a png").is_none());
    }

    #[test]
    fn png_len_walks_chunks() {
        assert_eq!(png_total_len(&TINY_PNG), Some(TINY_PNG.len()));
    }

    /// 构造一个只含 `eimg` 的最小 SFNT，验证偏移基准反推与帧切分。
    #[test]
    fn extracts_frames_from_minimal_eimg() {
        let count: u16 = 2;
        let header_len = 16usize + 4 * (count as usize + 1);
        let data_start = 32usize; // 偏移表之后留一点空隙模拟真实结构

        let mut eimg = vec![0u8; header_len];
        eimg[0] = 0x00;
        eimg[1] = 0x01; // version = 0x0001_0000
        eimg[4..6].copy_from_slice(&count.to_be_bytes());

        // 槽位 0 = PNG，槽位 1 = 非 PNG（应被跳过）
        let offs = [0u32, TINY_PNG.len() as u32, TINY_PNG.len() as u32 + 4];
        for (k, o) in offs.iter().enumerate() {
            let at = 16 + 4 * k;
            eimg[at..at + 4].copy_from_slice(&o.to_be_bytes());
        }
        eimg.resize(data_start, 0);
        eimg.extend_from_slice(&TINY_PNG);
        eimg.extend_from_slice(b"skip");

        let frames = extract_eimg_frames(&wrap_eimg(&eimg)).expect("extract");
        assert_eq!(frames.len(), 1, "only the PNG slot should be exported");
        assert_eq!(frames[0].slot, 0);
        assert_eq!((frames[0].width, frames[0].height), (1, 1));
        assert_eq!(frames[0].png, TINY_PNG);
    }

    /// 回归：20405 那类「数据区第 0 槽不是图片」的布局（修复前实测导出 0 帧）。
    ///
    /// 首个 PNG 落在第 1 槽，用「首帧签名 - offsets[0]」反推基准会整体高 32 字节，
    /// 整张表随之错位。这里靠表几何（table_end + 32）拿回正确基准。
    #[test]
    fn extracts_frames_when_first_slot_is_not_a_png() {
        let count: u16 = 3; // 槽 0 = 编排记录，槽 1/2 = 图片
        let table_end = 16usize + 4 * (count as usize + 1);
        let record = [0u8; INDEX_TAIL_LEN];

        let mut eimg = vec![0u8; table_end];
        eimg[1] = 0x01; // version = 0x0001_0000
        eimg[4..6].copy_from_slice(&count.to_be_bytes());

        // 偏移相对数据区起点：槽 0 是记录，槽 1、2 各一张图。
        let offs = [
            0u32,
            INDEX_TAIL_LEN as u32,
            (INDEX_TAIL_LEN + TINY_PNG.len()) as u32,
            (INDEX_TAIL_LEN + 2 * TINY_PNG.len()) as u32,
        ];
        for (k, o) in offs.iter().enumerate() {
            let at = 16 + 4 * k;
            eimg[at..at + 4].copy_from_slice(&o.to_be_bytes());
        }
        eimg.extend_from_slice(&record); // 表尾的编排记录（图片区之前）
        eimg.extend_from_slice(&record); // 数据区第 0 槽：又一条，不是图片
        eimg.extend_from_slice(&TINY_PNG);
        eimg.extend_from_slice(&TINY_PNG);

        let frames = extract_eimg_frames(&wrap_eimg(&eimg)).expect("extract");
        assert_eq!(frames.len(), 2, "both PNG slots should be exported");
        assert_eq!(
            frames.iter().map(|f| f.slot).collect::<Vec<_>>(),
            vec![1, 2],
            "slot numbers must survive the skipped record"
        );
        assert!(frames.iter().all(|f| f.png == TINY_PNG));
    }

    #[test]
    fn no_eimg_table_yields_no_frames() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());
        raw.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        assert!(extract_eimg_frames(&raw).unwrap().is_empty());
    }
}
