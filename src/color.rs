//! 把 FTF 私有彩色表变成标准 OpenType 彩色字体表（`COLR` v1 + `CPAL` v0）。
//!
//! ## 数据来源
//!
//! - `brsh`：**笔刷表**，一款字体里所有可用的「颜料」——单色或线性渐变色带。
//!   结构（实测 54981 / 23161 / 20405）：
//!   ```text
//!   u32 version (=0x00010000)
//!   u32 count
//!   u32[count + 1] 记录偏移（相对 data_start）
//!   data_start = 8 + 4 * (count + 1)
//!   记录:
//!     len == 5  : { u8 type, u32 ARGB }                      单色
//!     len >= 12 : { 12 字节头, u32[n] ARGB, u16[n] 位置 }     渐变
//!                 n = (len - 12) / 6，位置是 Q8.6（0..16384，16384 == 1.0）
//!   ```
//! - `cglf`：每字形一个 u16 的「选笔索引」。此字段的**精确语义尚未逆向闭环**，
//!   这里只把它当作选择一个 `brsh` 颜料的近似依据；取不到时退化为按 GID 轮转。
//!
//! ## 输出
//!
//! - `CPAL`：把所有 `brsh` 颜色收进 1 个调色板。
//! - `COLR` v1：每个非空字形一条 `PaintGlyph`，内部指向
//!   `PaintLinearGradient`（按该字形自身 bbox 竖向适配，与 QQ「一字一渐变」一致）
//!   或 `PaintSolid`。
//!
//! 这里不追求与 QQ 客户端 1:1，只要求视觉合理且能被 OTS / Chromium 接受。

use std::collections::HashMap;

/// F2DOT14 里 1.0 的定点值，同时也是 `brsh` 位置字段的满量程。
const F2DOT14_ONE: u16 = 16384;

/// 一种颜料。
#[derive(Debug, Clone)]
pub enum Paint {
    /// 单色，值为 ARGB。
    Solid(u32),
    /// 线性渐变色带，元素为 (位置 Q8.6 0..16384, ARGB)。
    Linear(Vec<(u16, u32)>),
}

impl Paint {
    fn is_visible(&self) -> bool {
        match self {
            Paint::Solid(c) => alpha(*c) != 0,
            Paint::Linear(stops) => stops.iter().any(|(_, c)| alpha(*c) != 0),
        }
    }

    fn colors(&self) -> Vec<u32> {
        match self {
            Paint::Solid(c) => vec![*c],
            Paint::Linear(stops) => stops.iter().map(|(_, c)| *c).collect(),
        }
    }
}

#[inline]
fn alpha(argb: u32) -> u8 {
    ((argb >> 24) & 0xFF) as u8
}

/// 解析 `brsh` 表；结构不符的记录会被跳过。
pub fn parse_brsh(blob: &[u8]) -> Vec<Paint> {
    if blob.len() < 8 {
        return Vec::new();
    }
    let count = u32::from_be_bytes([blob[4], blob[5], blob[6], blob[7]]) as usize;
    let data_start = 8 + 4 * (count + 1);
    if count == 0 || blob.len() < data_start {
        return Vec::new();
    }

    let offset_of = |k: usize| -> usize {
        let o = 8 + 4 * k;
        u32::from_be_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]) as usize
    };

    let mut paints = Vec::new();
    for i in 0..count {
        let s = data_start + offset_of(i);
        let e = data_start + offset_of(i + 1);
        if s >= e || e > blob.len() {
            continue;
        }
        let rec = &blob[s..e];
        if rec.len() == 5 && rec[0] == 0 {
            paints.push(Paint::Solid(u32::from_be_bytes([
                rec[1], rec[2], rec[3], rec[4],
            ])));
        } else if rec.len() >= 18 && (rec.len() - 12).is_multiple_of(6) {
            let n = (rec.len() - 12) / 6;
            let mut stops = Vec::with_capacity(n);
            for j in 0..n {
                let co = 12 + 4 * j;
                let po = 12 + 4 * n + 2 * j;
                let color = u32::from_be_bytes([
                    rec[co],
                    rec[co + 1],
                    rec[co + 2],
                    rec[co + 3],
                ]);
                let pos = u16::from_be_bytes([rec[po], rec[po + 1]]);
                stops.push((pos, color));
            }
            paints.push(Paint::Linear(stops));
        }
    }
    paints
}

/// 解析 `cglf`，返回**每个字形用哪支笔刷**（`None` = 这个字形不上色）。
///
/// ## 结构（实测 54981 / 23161）
///
/// ```text
/// 16B  头  { u32 version=0x00010000, u32 numGlyphs, u32 0x0000FF00, u32 1 }
/// u16[numGlyphs]        每字形所属「组」的编号（0..maxGroup，按 gid 连续分块）
/// 50B  子头            25 × u16，值恒等于 maxGroup
/// 组记录[maxGroup+1]   每组 8B：{ u16 0x4000, u16 笔刷<<8, u16 0x4000, u16 笔刷<<8 }
/// ```
///
/// 记录全 0 ⇒ 该组不上色。实测：54981 只有组 0..45 非零（= gid 0..101，即 ASCII +
/// Latin-1），笔刷 0（红 `#BF3E1E`）；23161 组 0..753 非零，笔刷 0..5（正好对应它的
/// 6 条渐变），CJK 组全 0 —— 与 QQ 里「数字/字母有色、汉字无色」完全一致。
///
/// 记录布局识别不出来时返回 `None`，调用方回退到「所有字形都上色」。
pub fn parse_cglf_brush(blob: &[u8], num_glyphs: usize) -> Option<Vec<Option<u8>>> {
    if blob.len() < 16 {
        return None;
    }
    let version = u32::from_be_bytes([blob[0], blob[1], blob[2], blob[3]]);
    if version != 0x0001_0000 {
        return None;
    }
    let ng = u32::from_be_bytes([blob[4], blob[5], blob[6], blob[7]]) as usize;
    if ng != num_glyphs || 16 + 2 * ng > blob.len() {
        return None;
    }
    let tail = &blob[16 + 2 * ng..];
    const SUB_HEADER: usize = 50;
    const REC: usize = 8;
    if tail.len() < SUB_HEADER + REC || !(tail.len() - SUB_HEADER).is_multiple_of(REC) {
        return None;
    }
    let records = (tail.len() - SUB_HEADER) / REC;
    let group = |gid: usize| -> usize {
        let o = 16 + 2 * gid;
        u16::from_be_bytes([blob[o], blob[o + 1]]) as usize
    };
    // 组编号必须能直接当记录下标用；像 20405 那样带高位标志的（值域 0xFF00+）就放弃。
    // 实测记录数恰好等于 maxGroup（54981: 92、23161: 1508），最后一个组没有记录 ⇒ 不上色。
    let max_group = (0..ng).map(group).max()?;
    if max_group > records {
        return None;
    }
    let brush_of = |i: usize| -> Option<u8> {
        if i >= records {
            return None;
        }
        let r = &tail[SUB_HEADER + i * REC..SUB_HEADER + (i + 1) * REC];
        if r.iter().all(|&b| b == 0) {
            return None;
        }
        let a = u16::from_be_bytes([r[2], r[3]]);
        let b = u16::from_be_bytes([r[6], r[7]]);
        Some((a.max(b) >> 8) as u8)
    };
    let out: Vec<Option<u8>> = (0..ng).map(|gid| brush_of(group(gid))).collect();
    if out.iter().all(|b| b.is_none()) {
        return None;
    }
    Some(out)
}

/// 生成的彩色表。
pub struct ColorTables {
    pub colr: Vec<u8>,
    pub cpal: Vec<u8>,
}

struct Palette {
    index: HashMap<u32, u16>,
    colors: Vec<u32>,
}

impl Palette {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            colors: Vec::new(),
        }
    }

    fn intern(&mut self, color: u32) -> u16 {
        if let Some(&i) = self.index.get(&color) {
            return i;
        }
        let i = self.colors.len() as u16;
        self.colors.push(color);
        self.index.insert(color, i);
        i
    }
}

/// 由 `brsh` / `cglf` 与字形 bbox 生成 `COLR` v1 与 `CPAL` v0。
///
/// 返回 `None` 表示没有可用的彩色数据（例如普通字体，或全部颜料透明）。
pub fn build_color_tables(
    paints: &[Paint],
    cglf_brush: Option<&[Option<u8>]>,
    bboxes: &[Option<(i16, i16, i16, i16)>],
    num_glyphs: usize,
    forced_gids: &[u16],
) -> Option<ColorTables> {
    let usable: Vec<usize> = paints
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_visible())
        .map(|(i, _)| i)
        .collect();
    if usable.is_empty() {
        return None;
    }

    // 有渐变时优先用渐变：这类「炫彩字体」单色里往往混着黑底/透明底，
    // 直接轮转会让大量字形变成黑色，观感很差。只有纯单色字体才用单色。
    let gradient_pool: Vec<usize> = usable
        .iter()
        .copied()
        .filter(|i| matches!(paints[*i], Paint::Linear(_)))
        .collect();
    let pool: &[usize] = if gradient_pool.is_empty() {
        &usable
    } else {
        &gradient_pool
    };

    // 选出要上色的字形，并确定它用哪个颜料。
    //
    // 有 `cglf` 的逐组记录时用它精确选色（哪些字形有色 + 用哪支笔刷）；
    // 拿不到时回退到「有轮廓就上色」+ 按 GID 轮转。
    let mut chosen: Vec<(u16, usize)> = Vec::new();
    match cglf_brush {
        Some(map) => {
            for gid in 0..num_glyphs.min(bboxes.len()).min(map.len()) {
                if bboxes[gid].is_none() {
                    continue;
                }
                let Some(b) = map[gid] else { continue };
                let b = b as usize;
                // 笔刷索引越界或本身就是透明的（如 23161 的第 7 支）⇒ 不上色。
                if b < paints.len() && paints[b].is_visible() {
                    chosen.push((gid as u16, b));
                }
            }
        }
        None => {
            for gid in 0..num_glyphs.min(bboxes.len()) {
                if bboxes[gid].is_none() {
                    continue;
                }
                chosen.push((gid as u16, pool[gid % pool.len()]));
            }
        }
    }
    // 外部指定的额外字形（QQ 皮肤端的逐字清单）：用第一支可见笔刷，与字母区一致。
    if let Some(fallback) = paints.iter().position(|p| p.is_visible()) {
        for &gid in forced_gids {
            let g = usize::from(gid);
            if g < bboxes.len()
                && bboxes[g].is_some()
                && !chosen.iter().any(|(already, _)| *already == gid)
            {
                chosen.push((gid, fallback));
            }
        }
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.sort_by_key(|(gid, _)| *gid);

    // 先登记所有会出现的颜色，得到稳定的调色板。
    let mut palette = Palette::new();
    for (_, pi) in &chosen {
        for c in paints[*pi].colors() {
            palette.intern(c);
        }
    }

    let colr = encode_colr(&chosen, paints, &mut palette, bboxes);
    let cpal = encode_cpal(&palette.colors);

    Some(ColorTables { colr, cpal })
}

/// 组装 `COLR` v1。
///
/// ```text
/// header (34 bytes)
/// BaseGlyphList  { u32 num; BaseGlyphPaintRecord[num] { u16 gid, Offset32 paint } }
/// paint blocks   （每块按 gid 升序紧密排列）
/// ```
fn encode_colr(
    chosen: &[(u16, usize)],
    paints: &[Paint],
    palette: &mut Palette,
    bboxes: &[Option<(i16, i16, i16, i16)>],
) -> Vec<u8> {
    const HEADER: usize = 34;
    let n = chosen.len();
    let bgl_len = 4 + n * 6;

    // 先编码所有 paint block，并记录它在 BaseGlyphList 之后的相对偏移。
    let mut paint_area: Vec<u8> = Vec::new();
    let mut records: Vec<(u16, usize)> = Vec::with_capacity(n);
    for &(gid, pi) in chosen {
        let off = paint_area.len();
        let bbox = bboxes[gid as usize].unwrap_or((0, 0, 0, 0));
        let block = encode_paint_block(gid, &paints[pi], palette, bbox);
        paint_area.extend_from_slice(&block);
        records.push((gid, off));
    }

    let mut out = Vec::with_capacity(HEADER + bgl_len + paint_area.len());
    // COLR v1 header
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
    out.extend_from_slice(&0u32.to_be_bytes()); // baseGlyphRecordsOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // layerRecordsOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    out.extend_from_slice(&(HEADER as u32).to_be_bytes()); // baseGlyphListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // varIndexMapOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // itemVariationStoreOffset
    debug_assert_eq!(out.len(), HEADER);

    // BaseGlyphList
    out.extend_from_slice(&(n as u32).to_be_bytes());
    for &(gid, off) in &records {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&((bgl_len + off) as u32).to_be_bytes());
    }
    out.extend_from_slice(&paint_area);
    out
}

/// 一个字形一层的 paint：`PaintGlyph -> (PaintLinearGradient | PaintSolid)`。
fn encode_paint_block(gid: u16, paint: &Paint, palette: &mut Palette, bbox: (i16, i16, i16, i16)) -> Vec<u8> {
    let mut out = Vec::new();
    // PaintGlyph (format 10): format(1) + Offset24(3) + u16 glyphID(2) = 6 字节
    out.push(10);
    out.extend_from_slice(&u24(6)); // 指向紧随其后的子 paint
    out.extend_from_slice(&gid.to_be_bytes());

    match paint {
        Paint::Solid(argb) => {
            // PaintSolid (format 2): format, u16 paletteIndex, F2DOT14 alpha
            // 透明度已经放在 CPAL 的颜色记录里，这里固定 1.0 避免二次衰减。
            out.push(2);
            out.extend_from_slice(&palette.intern(*argb).to_be_bytes());
            out.extend_from_slice(&F2DOT14_ONE.to_be_bytes());
        }
        Paint::Linear(stops) => {
            let (x0, y0, x1, y1, x2, y2) = gradient_coords(bbox);
            // PaintLinearGradient (format 4): format, Offset24 colorLine, int16 x0..y2
            out.push(4);
            out.extend_from_slice(&u24(16)); // ColorLine 紧跟 16 字节之后
            for v in [x0, y0, x1, y1, x2, y2] {
                out.extend_from_slice(&v.to_be_bytes());
            }
            // ColorLine: u8 extend, u16 numStops,
            //   ColorStop[] { F2DOT14 stopOffset, u16 paletteIndex, F2DOT14 alpha }
            // 注意 ColorStop 是 6 字节（含 alpha），不是 4 字节。
            out.push(0); // extend = PAD
            out.extend_from_slice(&(stops.len() as u16).to_be_bytes());
            for &(pos, color) in stops {
                let pos = pos.min(F2DOT14_ONE);
                out.extend_from_slice(&pos.to_be_bytes());
                out.extend_from_slice(&palette.intern(color).to_be_bytes());
                out.extend_from_slice(&F2DOT14_ONE.to_be_bytes()); // alpha = 1.0
            }
        }
    }
    out
}

/// 把一个字形的 bbox 映射成渐变的三个控制点。
///
/// - `p0 -> p1`：色带方向，stop 0 在字形顶部、stop 1 在底部（TrueType 里 y 轴向上）。
/// - `p0 -> p2`：参考方向；色带沿它的**垂线**推进，所以 `p2` 放在左下会让渐变
///   自右下向左上偏斜（顶部偏左、底部偏右）。
fn gradient_coords(bbox: (i16, i16, i16, i16)) -> (i16, i16, i16, i16, i16, i16) {
    let (x_min, y_min, x_max, y_max) = bbox;
    let cx = x_min + (x_max - x_min) / 2;
    (cx, y_max, cx, y_min, x_min, y_min)
}

/// 组装 `CPAL` v0（单调色板，颜色记录为 BGRA）。
fn encode_cpal(colors: &[u32]) -> Vec<u8> {
    let n = colors.len() as u16;
    let header = 12usize + 2; // 固定头 + 1 个调色板索引
    let mut out = Vec::with_capacity(header + colors.len() * 4);
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&n.to_be_bytes()); // numPaletteEntries
    out.extend_from_slice(&1u16.to_be_bytes()); // numPalettes
    out.extend_from_slice(&n.to_be_bytes()); // numColorRecords
    out.extend_from_slice(&(header as u32).to_be_bytes()); // colorRecordsArrayOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0]
    for &argb in colors {
        let a = ((argb >> 24) & 0xFF) as u8;
        let r = ((argb >> 16) & 0xFF) as u8;
        let g = ((argb >> 8) & 0xFF) as u8;
        let b = (argb & 0xFF) as u8;
        out.extend_from_slice(&[b, g, r, a]);
    }
    out
}

/// 写入 3 字节大端偏移（`Offset24`）。
fn u24(v: usize) -> [u8; 3] {
    let v = v as u32;
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(argb: u32) -> Vec<u8> {
        let mut v = vec![0u8];
        v.extend_from_slice(&argb.to_be_bytes());
        v
    }

    #[test]
    fn brsh_parses_solids_and_gradients() {
        // 2 条记录：一条单色、一条 2 色标渐变
        let mut recs = Vec::new();
        recs.push(solid(0xFF11_2233));
        let mut grad = vec![4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
        grad.extend_from_slice(&0xFF00_00FFu32.to_be_bytes());
        grad.extend_from_slice(&0xFF00_FF00u32.to_be_bytes());
        grad.extend_from_slice(&0u16.to_be_bytes());
        grad.extend_from_slice(&F2DOT14_ONE.to_be_bytes());
        recs.push(grad);

        let mut blob = Vec::new();
        blob.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        blob.extend_from_slice(&2u32.to_be_bytes());
        let mut offsets = Vec::with_capacity(recs.len() + 1);
        let mut acc = 0u32;
        for r in &recs {
            offsets.push(acc);
            acc += r.len() as u32;
        }
        offsets.push(acc);
        for o in offsets {
            blob.extend_from_slice(&o.to_be_bytes());
        }
        for r in &recs {
            blob.extend_from_slice(r);
        }

        let paints = parse_brsh(&blob);
        assert_eq!(paints.len(), 2);
        assert!(matches!(paints[0], Paint::Solid(0xFF11_2233)));
        match &paints[1] {
            Paint::Linear(stops) => {
                assert_eq!(stops.len(), 2);
                assert_eq!(stops[0], (0, 0xFF00_00FF));
                assert_eq!(stops[1], (F2DOT14_ONE, 0xFF00_FF00));
            }
            _ => panic!("expected gradient"),
        }
    }

    #[test]
    fn color_tables_build() {
        let paints = vec![
            Paint::Linear(vec![(0, 0xFF00_00FF), (F2DOT14_ONE, 0xFFFF_0000)]),
            Paint::Solid(0x0000_0000), // 透明，应被过滤
        ];
        let bboxes = vec![Some((0, 0, 100, 100)), None, Some((10, 20, 30, 40))];
        let tables = build_color_tables(&paints, None, &bboxes, 3, &[]).expect("tables");
        // COLR 头版本为 1
        assert_eq!(&tables.colr[..2], &[0, 1]);
        // BaseGlyphList 的记录数：只有 2 个字形有轮廓
        let bgl = u32::from_be_bytes([
            tables.colr[34],
            tables.colr[35],
            tables.colr[36],
            tables.colr[37],
        ]);
        assert_eq!(bgl, 2);
        // CPAL 调色板恰好 2 个颜色
        assert_eq!(u16::from_be_bytes([tables.cpal[2], tables.cpal[3]]), 2);
    }

    fn rec(brush: u16) -> [u8; 8] {
        let mut r = [0u8; 8];
        r[0..2].copy_from_slice(&0x4000u16.to_be_bytes());
        r[2..4].copy_from_slice(&(brush << 8).to_be_bytes());
        r[4..6].copy_from_slice(&0x4000u16.to_be_bytes());
        r[6..8].copy_from_slice(&(brush << 8).to_be_bytes());
        r
    }

    #[test]
    fn cglf_brush_map_picks_per_group() {
        // 4 个字形，组 = [0,0,1,1]；组 0 的记录选了笔刷 3，组 1 全零（不上色）。
        let ng = 4usize;
        let mut blob = Vec::new();
        blob.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        blob.extend_from_slice(&(ng as u32).to_be_bytes());
        blob.extend_from_slice(&0x0000_FF00u32.to_be_bytes());
        blob.extend_from_slice(&1u32.to_be_bytes());
        for g in [0u16, 0, 1, 1] {
            blob.extend_from_slice(&g.to_be_bytes());
        }
        // 子头 50B（值恒为 maxGroup），随后 maxGroup+1 = 2 条记录。
        for _ in 0..25 {
            blob.extend_from_slice(&1u16.to_be_bytes());
        }
        blob.extend_from_slice(&rec(3));
        blob.extend_from_slice(&[0u8; 8]);

        let map = parse_cglf_brush(&blob, ng).expect("brush map");
        assert_eq!(map, vec![Some(3), Some(3), None, None]);
    }

    #[test]
    fn cglf_brush_map_rejects_flagged_groups() {
        // 组值带高位标志（20405 那类）无法直接当记录下标 ⇒ 放弃，回退全字形上色。
        let ng = 2usize;
        let mut blob = Vec::new();
        blob.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        blob.extend_from_slice(&(ng as u32).to_be_bytes());
        blob.extend_from_slice(&0x0000_FF00u32.to_be_bytes());
        blob.extend_from_slice(&1u32.to_be_bytes());
        for g in [0xFF00u16, 0xFF01] {
            blob.extend_from_slice(&g.to_be_bytes());
        }
        for _ in 0..25 {
            blob.extend_from_slice(&0u16.to_be_bytes());
        }
        blob.extend_from_slice(&rec(1));
        assert!(parse_cglf_brush(&blob, ng).is_none());
    }

    #[test]
    fn color_tables_use_explicit_brush_map() {
        let paints = vec![
            Paint::Solid(0xFFBF_3E1E),
            Paint::Linear(vec![(0, 0xFF00_00FF), (F2DOT14_ONE, 0xFFFF_0000)]),
        ];
        let bboxes = vec![Some((0, 0, 100, 100)); 4];
        let map = vec![Some(0), None, Some(1), Some(9)]; // 9 越界 ⇒ 跳过
        let tables = build_color_tables(&paints, Some(&map), &bboxes, 4, &[]).expect("tables");
        let bgl = u32::from_be_bytes([
            tables.colr[34],
            tables.colr[35],
            tables.colr[36],
            tables.colr[37],
        ]);
        assert_eq!(bgl, 2); // 只有 gid 0 与 gid 2
    }

    #[test]
    fn forced_gids_use_first_visible_paint() {
        let paints = vec![
            Paint::Solid(0x0000_0000), // 透明
            Paint::Solid(0xFFBF_3E1E),
        ];
        let bboxes = vec![Some((0, 0, 100, 100)); 3];
        // 空 cglf map（全部不上色）+ 强制 gid 2；重复项与越界项都不应重复入表。
        let empty = vec![None; 3];
        let tables =
            build_color_tables(&paints, Some(&empty), &bboxes, 3, &[2, 2, 99]).expect("tables");
        let bgl = u32::from_be_bytes([
            tables.colr[34],
            tables.colr[35],
            tables.colr[36],
            tables.colr[37],
        ]);
        assert_eq!(bgl, 1);
        // CPAL 里只有那支不透明的颜色
        assert_eq!(u16::from_be_bytes([tables.cpal[2], tables.cpal[3]]), 1);
    }
}
