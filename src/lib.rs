use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, HashMap};

mod assets;
mod color;
pub mod ots;

pub use assets::{extract_eimg_frames, EimgFrame};

const X_OFF: i32 = 128;
const Y_OFF: i32 = 92;

/// 转换开关。`Default` 保持与旧版 `convert_ftf` 完全一致（不带彩色）。
#[derive(Debug, Clone, Default)]
pub struct ConvertOptions {
    /// 生成彩色字体：从 `brsh` / `cglf` 私有表推导出 `COLR` v1 + `CPAL` v0，
    /// 并丢弃随之无用的私有表。
    pub color: bool,
    /// 额外强制上色的字符。
    ///
    /// `cglf` 的组记录只覆盖「数字/字母」这类成组的字形；QQ 皮肤里还有一小撮
    /// 零散汉字（如 54981 的 `想生联合狩猎塔罗之`）会被上色，而这份逐字清单
    /// **不在字体文件里**（`name`/`post`/`csty`/`assy`/`sgrp`/注解表全部排查过）。
    /// 想 1:1 复现时把那些字符传进来即可。
    pub color_chars: Option<Vec<char>>,
}

/// 这些是 QQ 私有表。开启彩色输出时它们已被转换成标准 `COLR`/`CPAL`
/// （或是与静态字形无关的动画/场景数据），因此不再保留。
fn is_private_color_table(tag: &[u8]) -> bool {
    [
        b"brsh", b"cglf", b"eimg", b"scen", b"asst", b"smap", b"fpid",
        b"ganm", b"vgrp", b"vsty", b"csty", b"assy", b"sgrp",
    ]
    .iter()
    .any(|t| t.as_slice() == tag)
}

#[derive(Debug, Clone)]
struct SubRecord {
    points: Vec<(i32, i32)>,
    flags: Vec<u8>,
    c1: usize,
    c2: usize,
    extra: Vec<u8>,
    reverse: bool,
    chain: [i32; 7],
}

#[derive(Debug, Clone, Copy)]
enum GlyphState {
    Unvisited,
    Visiting,
    Visited,
}

struct FtfParser<'a> {
    ftfg: &'a [u8],
    loca_vals: Vec<usize>,
    mode1: u8,
    mode2: u8,
    memo: Vec<Option<Vec<SubRecord>>>,
    state: Vec<GlyphState>,
}

impl<'a> FtfParser<'a> {
    fn new(ftfg: &'a [u8], loca_vals: Vec<usize>, num_glyphs: usize, mode1: u8, mode2: u8) -> Self {
        Self {
            ftfg,
            loca_vals,
            mode1,
            mode2,
            memo: vec![None; num_glyphs],
            state: vec![GlyphState::Unvisited; num_glyphs],
        }
    }

    #[inline(always)]
    fn read_coord(r: &[u8], pos: &mut usize, mode: u8) -> Result<i32> {
        if mode == 1 {
            if *pos + 1 > r.len() {
                bail!("Unexpected EOF reading 1-byte coord");
            }
            let val = r[*pos] as i8 as i32;
            *pos += 1;
            Ok(val)
        } else {
            if *pos + 2 > r.len() {
                bail!("Unexpected EOF reading 2-byte coord");
            }
            let val = i16::from_be_bytes([r[*pos], r[*pos + 1]]) as i32;
            *pos += 2;
            Ok(val)
        }
    }

    fn read_transform(&self, r: &[u8], pos: &mut usize) -> Result<([i32; 7], u8)> {
        if *pos >= r.len() {
            bail!("Unexpected EOF reading transform flags");
        }
        let v6 = r[*pos];
        *pos += 1;
        let mut m = [64, 0, 0, 64, 0, 0, 64];

        let specs = [
            (1, 0, self.mode1),
            (2, 1, self.mode1),
            (4, 2, self.mode1),
            (8, 3, self.mode1),
            (16, 4, self.mode2),
            (32, 5, self.mode2),
            (64, 6, self.mode1),  // extra 参数
        ];

        for (bit, idx, mode) in specs {
            if (v6 & bit) != 0 {
                m[idx] = Self::read_coord(r, pos, mode)?;
            }
        }
        m[4] <<= 6;
        m[5] <<= 6;
        Ok((m, v6))
    }

    #[inline(always)]
    fn compose(parent: &[i32; 7], child: &[i32; 7]) -> [i32; 7] {
        [
            (parent[0] * child[0] + parent[2] * child[1]) >> 6,
            (parent[1] * child[0] + parent[3] * child[1]) >> 6,
            (parent[0] * child[2] + parent[2] * child[3]) >> 6,
            (parent[1] * child[2] + parent[3] * child[3]) >> 6,
            parent[4] + ((parent[0] * child[4] + parent[2] * child[5]) >> 6),
            parent[5] + ((parent[1] * child[4] + parent[3] * child[5]) >> 6),
            (parent[6] * child[6]) >> 6,
        ]
    }

    fn skip_extended_component(&self, r: &[u8], pos: &mut usize) -> Result<()> {
        // 跳过扩展组件的元素列表（当 transform.flags & 0x80）
        loop {
            if *pos >= r.len() {
                bail!("Unexpected EOF in extended component");
            }
            let v52 = r[*pos];
            let mut _count = (v52 & 0x3F) as usize;
            *pos += 1;

            if (v52 & 0x40) != 0 {
                if *pos >= r.len() {
                    bail!("Unexpected EOF reading extended count");
                }
                _count |= (r[*pos] as usize) << 8;
                *pos += 1;
            }

            if *pos >= r.len() {
                bail!("Unexpected EOF reading element header");
            }
            let v56 = r[*pos];
            *pos += 1;

            let case = v56 >> 5;
            let ngrp = (v56 & 7) as usize;

            // 跳过 case 对应的数据
            if case == 7 {
                // full transform
                let (_, _flags) = self.read_transform(r, pos)?;
            } else if case == 2 || case == 3 || case == 6 {
                // sx/sy/extra: mode1
                Self::read_coord(r, pos, self.mode1)?;
            } else if case == 4 || case == 5 {
                // tx/ty: mode2 (will be <<6)
                Self::read_coord(r, pos, self.mode2)?;
            }

            // 跳过 group 数据
            for _ in 0..ngrp {
                if *pos >= r.len() {
                    bail!("Unexpected EOF in group");
                }
                let v74 = r[*pos];
                *pos += 1;

                if (v56 & 8) != 0 {
                    // extended group index
                    if *pos >= r.len() {
                        bail!("Unexpected EOF in extended group index");
                    }
                    *pos += 1;
                }

                let cmode = v74 >> 6;
                let w = (v74 >> 4) & 3;

                // 跳过坐标数据
                if cmode == 2 {
                    if w == 0 {
                        Self::read_coord(r, pos, self.mode2)?;
                        Self::read_coord(r, pos, self.mode2)?;
                    } else if w == 1 || w == 2 || w == 3 {
                        Self::read_coord(r, pos, self.mode2)?;
                    }
                } else if cmode == 3 {
                    Self::read_coord(r, pos, self.mode2)?;
                    Self::read_coord(r, pos, self.mode2)?;
                }
            }

            // 检查是否有更多元素
            if (v52 & 0x80) == 0 {
                break;
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn apply_matrix(p: (i32, i32), m: &[i32; 7]) -> (i32, i32) {
        let (x, y) = p;
        (
            (m[0] * x + m[2] * y + m[4]) >> 6,
            (m[1] * x + m[3] * y + m[5]) >> 6,
        )
    }

    fn raw_outline(&mut self, gid: usize) -> Result<Vec<SubRecord>> {
        if gid >= self.loca_vals.len() - 1 || gid >= self.memo.len() {
            // 超出范围的 GID：返回空记录（与 Python 版本一致）
            return Ok(Vec::new());
        }
        match self.state[gid] {
            GlyphState::Visiting => bail!("Glyph reference cycle detected at GID {}", gid),
            GlyphState::Visited => return Ok(self.memo[gid].clone().unwrap_or_default()),
            GlyphState::Unvisited => {}
        }

        self.state[gid] = GlyphState::Visiting;
        let s = self.loca_vals[gid];
        let e = self.loca_vals[gid + 1];
        let mut out = Vec::new();

        if e > s && e <= self.ftfg.len() {
            let r = &self.ftfg[s..e];
            let mut pos = 0;
            while pos < r.len() {
                let b0 = r[pos];
                if (b0 & 0x80) != 0 {
                    if pos + 4 > r.len() {
                        bail!("Corrupted simple glyph header at GID {}", gid);
                    }
                    let c1 = r[pos + 1] as usize;
                    let c2 = u16::from_be_bytes([r[pos + 2], r[pos + 3]]) as usize;
                    let total = c1 + c2;
                    pos += 4;

                    let mut pts = Vec::with_capacity(total);
                    for _ in 0..total {
                        let x = Self::read_coord(r, &mut pos, self.mode2)?;
                        let y = Self::read_coord(r, &mut pos, self.mode2)?;
                        pts.push((x, y));
                    }

                    if pos + total > r.len() {
                        bail!("Corrupted simple glyph flags at GID {}", gid);
                    }
                    let flags = r[pos..pos + total].to_vec();
                    pos += total;

                    let mut extra = Vec::new();
                    if c1 > 0 {
                        if pos + c2 > r.len() { bail!("Corrupted extra bytes at GID {}", gid); }
                        extra.extend_from_slice(&r[pos..pos + c2]);
                        pos += c2;
                    }
                    out.push(SubRecord {
                        points: pts,
                        flags,
                        c1,
                        c2,
                        extra,
                        reverse: false,
                        chain: [64, 0, 0, 64, 0, 0, 64],
                    });
                    if (b0 & 0x40) == 0 {
                        break;
                    }
                } else {
                    let nb = (b0 & 7) as usize;
                    if pos + 1 + nb > r.len() {
                        bail!("Corrupted component GID offset at GID {}", gid);
                    }
                    let mut child_gid = 0usize;
                    for i in 0..nb {
                        child_gid = (child_gid << 8) | (r[pos + 1 + i] as usize);
                    }
                    pos += 1 + nb;
                    let (m, v6) = self.read_transform(r, &mut pos)?;

                    // 如果 transform flags bit7 被设置，跳过扩展组件的元素列表
                    if (v6 & 0x80) != 0 {
                        self.skip_extended_component(r, &mut pos)?;
                    }

                    let child_records = self.raw_outline(child_gid)?;
                    for mut rec in child_records {

                        rec.chain = Self::compose(&m, &rec.chain);
                        out.push(rec);
                    }
                    if (b0 & 0x40) == 0 {
                        break;
                    }
                }
            }
        }

        self.state[gid] = GlyphState::Visited;
        self.memo[gid] = Some(out.clone());
        Ok(out)
    }

    pub fn parse_all(&mut self, num_glyphs: usize) -> Result<Vec<Vec<SubRecord>>> {
        let mut glyphs = Vec::with_capacity(num_glyphs);
        for gid in 0..num_glyphs {
            let recs = self.raw_outline(gid)?;
            let mut mapped_recs = Vec::with_capacity(recs.len());

            for rec in recs {
                let m = rec.chain;
                let c1 = rec.c1;
                let c2 = rec.c2;
                let extra_scale = m[6];

                let mut o_pts = Vec::new();
                let mut o_flags = Vec::new();

                if c1 > 0 {
                    let base: Vec<(i32, i32)> = rec.points[..c1]
                        .iter()
                        .map(|&p| Self::apply_matrix(p, &m))
                        .collect();

                    for i in 0..c2 {
                        let (dx, dy) = rec.points[c1 + i];
                        let ext_idx = rec.extra[i] as usize;
                        if ext_idx >= base.len() { continue; } // 防止越界
                        let (bx, by) = base[ext_idx];
                        o_pts.push((
                            bx + ((dx * extra_scale) >> 6),
                            by + ((dy * extra_scale) >> 6),
                        ));
                    }
                    o_flags.extend_from_slice(&rec.flags[c1..c1 + c2]);
                } else {
                    o_pts.extend(rec.points[..c2].iter().map(|&p| Self::apply_matrix(p, &m)));
                    o_flags.extend_from_slice(&rec.flags[..c2]);
                }

                if rec.reverse {
                    o_pts.reverse();
                    o_flags.reverse();
                }

                if let Some(last) = o_flags.last_mut() {
                    *last |= 0x80;
                }

                let final_pts = o_pts.into_iter().map(|(x, y)| (x + X_OFF, Y_OFF - y)).collect();

                mapped_recs.push(SubRecord {
                    points: final_pts,
                    flags: o_flags,
                    c1: 0,
                    c2: rec.c2,
                    extra: Vec::new(),
                    reverse: false,
                    chain: [64, 0, 0, 64, 0, 0, 64],
                });
            }
            glyphs.push(mapped_recs);
        }
        Ok(glyphs)
    }
}

struct ConvertedGlyph {
    data: Vec<u8>,
    pts_count: usize,
    contours_count: usize,
    bbox: Option<(i16, i16, i16, i16)>,
}

fn encode_simple_glyph(records: &[SubRecord]) -> ConvertedGlyph {
    let mut coords = Vec::new();
    let mut flags_arr = Vec::new();
    let mut endpts = Vec::new();

    for rec in records {
        let mut has_end = false;
        for (i, &p) in rec.points.iter().enumerate() {
            coords.push(p);
            flags_arr.push(rec.flags[i] & 1);
            if (rec.flags[i] & 0x80) != 0 {
                endpts.push(coords.len() - 1);
                has_end = true;
            }
        }
        if !rec.points.is_empty() && !has_end {
            endpts.push(coords.len() - 1);
        }
    }

    if coords.is_empty() || endpts.is_empty() {
        return ConvertedGlyph {
            data: Vec::new(),
            pts_count: 0,
            contours_count: 0,
            bbox: None,
        };
    }

    let num_contours = endpts.len() as i16;
    let mut x_min = i32::MAX;
    let mut x_max = i32::MIN;
    let mut y_min = i32::MAX;
    let mut y_max = i32::MIN;

    for &(x, y) in &coords {
        if x < x_min { x_min = x; }
        if x > x_max { x_max = x; }
        if y < y_min { y_min = y; }
        if y > y_max { y_max = y; }
    }

    let bbox = (x_min as i16, y_min as i16, x_max as i16, y_max as i16);

    let mut buf = Vec::new();
    buf.extend_from_slice(&num_contours.to_be_bytes());
    buf.extend_from_slice(&bbox.0.to_be_bytes());
    buf.extend_from_slice(&bbox.1.to_be_bytes());
    buf.extend_from_slice(&bbox.2.to_be_bytes());
    buf.extend_from_slice(&bbox.3.to_be_bytes());

    for &ep in &endpts {
        buf.extend_from_slice(&(ep as u16).to_be_bytes());
    }
    buf.extend_from_slice(&0u16.to_be_bytes());

    let mut flags_out = Vec::with_capacity(coords.len());
    let mut x_bytes = Vec::new();
    let mut y_bytes = Vec::new();
    let mut last_x = 0i32;
    let mut last_y = 0i32;

    for i in 0..coords.len() {
        let (x, y) = coords[i];
        let dx = x - last_x;
        let dy = y - last_y;
        last_x = x;
        last_y = y;

        let mut f = flags_arr[i];

        if dx == 0 {
            f |= 0x10;
        } else if (1..=255).contains(&dx) {
            f |= 0x02 | 0x10;
            x_bytes.push(dx as u8);
        } else if (-255..=-1).contains(&dx) {
            f |= 0x02;
            x_bytes.push((-dx) as u8);
        } else {
            x_bytes.extend_from_slice(&(dx as i16).to_be_bytes());
        }

        if dy == 0 {
            f |= 0x20;
        } else if (1..=255).contains(&dy) {
            f |= 0x04 | 0x20;
            y_bytes.push(dy as u8);
        } else if (-255..=-1).contains(&dy) {
            f |= 0x04;
            y_bytes.push((-dy) as u8);
        } else {
            y_bytes.extend_from_slice(&(dy as i16).to_be_bytes());
        }

        flags_out.push(f);
    }

    buf.extend_from_slice(&flags_out);
    buf.extend_from_slice(&x_bytes);
    buf.extend_from_slice(&y_bytes);

    ConvertedGlyph {
        data: buf,
        pts_count: coords.len(),
        contours_count: endpts.len(),
        bbox: Some(bbox),
    }
}

fn calc_table_checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    let chunks = data.chunks_exact(4);
    let rem = chunks.remainder();
    for chunk in chunks {
        sum = sum.wrapping_add(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if !rem.is_empty() {
        let mut last = [0u8; 4];
        last[..rem.len()].copy_from_slice(rem);
        sum = sum.wrapping_add(u32::from_be_bytes(last));
    }
    sum
}

fn fix_normal_ttf(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() < 12 {
        bail!("File too short to be a valid TTF");
    }
    let num_tables = u16::from_be_bytes([raw[4], raw[5]]) as usize;
    if raw.len() < 12 + num_tables * 16 {
        bail!("Corrupted table directory");
    }

    let mut orig_tables = HashMap::new();
    let mut has_empty_table = false;

    for i in 0..num_tables {
        let off = 12 + i * 16;
        let tag = &raw[off..off + 4];
        let toff = u32::from_be_bytes([raw[off + 8], raw[off + 9], raw[off + 10], raw[off + 11]]) as usize;
        let tlen = u32::from_be_bytes([raw[off + 12], raw[off + 13], raw[off + 14], raw[off + 15]]) as usize;
        if toff + tlen > raw.len() {
            bail!("Table {:?} points outside file boundary", std::str::from_utf8(tag));
        }
        let data = &raw[toff..toff + tlen];
        if data.is_empty() {
            has_empty_table = true;
        }
        orig_tables.insert(tag, data);
    }

    if !has_empty_table {
        return Ok(raw.to_vec());
    }

    let mut tables_map: BTreeMap<&[u8], Vec<u8>> = BTreeMap::new();
    for (tag, data) in &orig_tables {
        if !data.is_empty() {
            tables_map.insert(tag, data.to_vec());
        }
    }

    let out_num_tables = tables_map.len() as u16;
    let entry_selector = (out_num_tables as f64).log2().floor() as u16;
    let search_range = (1 << entry_selector) * 16;
    let range_shift = out_num_tables * 16 - search_range;

    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&out_num_tables.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());

    let header_size = 12 + out_num_tables as usize * 16;
    let mut current_offset = header_size;
    let mut dir_entries = Vec::new();
    let mut table_blobs = Vec::new();
    let mut head_table_file_offset = 0;

    for (tag, data) in tables_map {
        let checksum = calc_table_checksum(&data);
        let offset = current_offset as u32;
        let length = data.len() as u32;

        if tag == b"head" {
            head_table_file_offset = offset as usize;
        }

        dir_entries.push((tag, checksum, offset, length));
        table_blobs.push(data);

        let padded_len = (length as usize + 3) & !3;
        current_offset += padded_len;
    }

    for (tag, checksum, offset, length) in &dir_entries {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
    }

    for blob in table_blobs {
        out.extend_from_slice(&blob);
        let rem = blob.len() % 4;
        if rem != 0 {
            out.extend_from_slice(&vec![0u8; 4 - rem]);
        }
    }

    let whole_checksum = calc_table_checksum(&out);
    let check_sum_adj = 0xB1B0AFBA_u32.wrapping_sub(whole_checksum);
    if head_table_file_offset > 0 && head_table_file_offset + 12 <= out.len() {
        out[head_table_file_offset + 8..head_table_file_offset + 12]
            .copy_from_slice(&check_sum_adj.to_be_bytes());
    }

    Ok(out)
}

/// 重建 cmap 表：修复 format 4 子表中多个 0xFFFF 终止段的问题
/// （OTS 要求恰好一个以 0xFFFF 结尾的段，否则报
/// "cmap: multiple 0xffff terminators found"）。
fn rebuild_cmap_table(cmap_raw: &[u8]) -> Result<Vec<u8>> {
    if cmap_raw.len() < 4 {
        return Ok(cmap_raw.to_vec());
    }
    let version = u16::from_be_bytes([cmap_raw[0], cmap_raw[1]]);
    let ntab = u16::from_be_bytes([cmap_raw[2], cmap_raw[3]]) as usize;
    if cmap_raw.len() < 4 + ntab * 8 {
        return Ok(cmap_raw.to_vec());
    }

    let mut recs: Vec<(u16, u16, usize)> = Vec::with_capacity(ntab);
    let mut pos = 4;
    for _ in 0..ntab {
        let pid = u16::from_be_bytes([cmap_raw[pos], cmap_raw[pos + 1]]);
        let eid = u16::from_be_bytes([cmap_raw[pos + 2], cmap_raw[pos + 3]]);
        let off = u32::from_be_bytes([
            cmap_raw[pos + 4],
            cmap_raw[pos + 5],
            cmap_raw[pos + 6],
            cmap_raw[pos + 7],
        ]) as usize;
        recs.push((pid, eid, off));
        pos += 8;
    }

    let mut new_recs: Vec<(u16, u16, Vec<u8>)> = Vec::with_capacity(ntab);
    for (pid, eid, off) in recs {
        if off >= cmap_raw.len() {
            continue;
        }
        let sub = &cmap_raw[off..];
        let fmt = if sub.len() >= 2 { u16::from_be_bytes([sub[0], sub[1]]) } else { 0xFFFF };
        if fmt == 4 {
            new_recs.push((pid, eid, rebuild_format4_subtable(sub)?));
        } else {
            new_recs.push((pid, eid, sub.to_vec()));
        }
    }

    // 去重：多个编码记录可能共享同一份子表数据（原始字体用相同 offset 引用），
    // 这里按内容去重，避免输出时子表被重复写入导致体积翻倍
    let header_len = 4 + new_recs.len() * 8;
    let mut cur = header_len;
    let mut blob_offsets: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut offsets = Vec::with_capacity(new_recs.len());
    let mut blob_order: Vec<&Vec<u8>> = Vec::new();
    for (_, _, data) in &new_recs {
        if let Some(&o) = blob_offsets.get(data) {
            offsets.push(o);
        } else {
            blob_offsets.insert(data.clone(), cur);
            offsets.push(cur);
            blob_order.push(data);
            cur += (data.len() + 3) & !3;
        }
    }

    let mut out = Vec::with_capacity(cur);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&(new_recs.len() as u16).to_be_bytes());
    for ((pid, eid, _), &o) in new_recs.iter().zip(&offsets) {
        out.extend_from_slice(&pid.to_be_bytes());
        out.extend_from_slice(&eid.to_be_bytes());
        out.extend_from_slice(&(o as u32).to_be_bytes());
    }
    for data in blob_order {
        out.extend_from_slice(data);
        let rem = data.len() % 4;
        if rem != 0 {
            out.extend_from_slice(&[0u8; 3][..4 - rem]);
        }
    }
    Ok(out)
}

/// 在 `cmap` 里查一个码点对应的字形 id（支持 format 4 / 12），用于 `ConvertOptions::color_chars`。
fn cmap_lookup(cmap_raw: &[u8], cp: u32) -> Option<u16> {
    fn u16_at(b: &[u8], o: usize) -> Option<u16> {
        Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
    }
    fn u32_at(b: &[u8], o: usize) -> Option<u32> {
        Some(u32::from_be_bytes([
            *b.get(o)?,
            *b.get(o + 1)?,
            *b.get(o + 2)?,
            *b.get(o + 3)?,
        ]))
    }

    if cmap_raw.len() < 4 {
        return None;
    }
    let ntab = usize::from(u16_at(cmap_raw, 2)?);
    for i in 0..ntab {
        let rec = 4 + i * 8;
        let Some(off) = u32_at(cmap_raw, rec + 4).map(|o| o as usize) else {
            continue;
        };
        let Some(sub) = cmap_raw.get(off..) else {
            continue;
        };
        match u16_at(sub, 0) {
            Some(4) if cp <= 0xFFFF => {
                let (Some(length), Some(seg_x2)) = (u16_at(sub, 2), u16_at(sub, 6)) else {
                    continue;
                };
                let (length, seg_x2) = (usize::from(length), usize::from(seg_x2));
                let segments = seg_x2 / 2;
                if segments == 0 || length > sub.len() {
                    continue;
                }
                let (end_off, start_off) = (14usize, 16 + seg_x2);
                let (delta_off, range_off) = (start_off + seg_x2, start_off + 2 * seg_x2);
                let cp16 = cp as u16;
                for s in 0..segments {
                    let (Some(end), Some(start)) =
                        (u16_at(sub, end_off + s * 2), u16_at(sub, start_off + s * 2))
                    else {
                        break;
                    };
                    if cp16 < start || cp16 > end {
                        continue;
                    }
                    let delta = i16::from_be_bytes([
                        sub[delta_off + s * 2],
                        sub[delta_off + s * 2 + 1],
                    ]);
                    let ro = u16_at(sub, range_off + s * 2)?;
                    let gid = if ro == 0 {
                        cp16.wrapping_add(delta as u16)
                    } else {
                        let at = range_off + s * 2 + usize::from(ro) + 2 * usize::from(cp16 - start);
                        u16_at(sub, at)?.wrapping_add(delta as u16)
                    };
                    return (gid != 0).then_some(gid);
                }
            }
            Some(12) => {
                let ngroups = u32_at(sub, 12)? as usize;
                for g in 0..ngroups {
                    let o = 16 + g * 12;
                    let (Some(hi), Some(lo), Some(base)) =
                        (u32_at(sub, o), u32_at(sub, o + 4), u32_at(sub, o + 8))
                    else {
                        break;
                    };
                    if cp < hi || cp > lo {
                        continue;
                    }
                    return u16::try_from(base + (cp - hi)).ok().filter(|gid| *gid != 0);
                }
            }
            _ => {}
        }
    }
    None
}

/// 重建 cmap format 4 子表：解码为 (码点 -> glyph) 映射后重新编码。
/// 排除非字符 U+FFFF，并始终追加标准的纯终止段 [0xFFFF, 0xFFFF, 1]。
/// 若表本身已满足 OTS 要求（恰好一个 0xFFFF 终止段且在末尾），则原样返回。
fn rebuild_format4_subtable(sub: &[u8]) -> Result<Vec<u8>> {
    if sub.len() < 14 {
        return Ok(sub.to_vec());
    }
    let length = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let seg_x2 = u16::from_be_bytes([sub[6], sub[7]]) as usize;
    let segcount = seg_x2 / 2;
    if length > sub.len() || segcount == 0 {
        return Ok(sub.to_vec());
    }
    let end_off = 14usize;
    let start_off = end_off + seg_x2 + 2; // reservedPad
    let delta_off = start_off + seg_x2;
    let range_off = delta_off + seg_x2;
    let glyph_off = range_off + seg_x2;
    if glyph_off > length {
        return Ok(sub.to_vec());
    }

    // 若恰好一个 0xFFFF 终止段且在最后，则无需重建
    let mut nffff = 0usize;
    for i in 0..segcount {
        let e = u16::from_be_bytes([sub[end_off + i * 2], sub[end_off + i * 2 + 1]]);
        if e == 0xFFFF {
            nffff += 1;
        }
    }
    if nffff == 1 {
        let last_end = u16::from_be_bytes([
            sub[end_off + (segcount - 1) * 2],
            sub[end_off + (segcount - 1) * 2 + 1],
        ]);
        if last_end == 0xFFFF {
            return Ok(sub.to_vec());
        }
    }

    // 解码 (码点 -> glyph) 映射，首个匹配的段优先
    let mut mapping: BTreeMap<u16, u16> = BTreeMap::new();
    let glyph_arr = &sub[glyph_off..length];
    for i in 0..segcount {
        let start = u16::from_be_bytes([sub[start_off + i * 2], sub[start_off + i * 2 + 1]]);
        let end = u16::from_be_bytes([sub[end_off + i * 2], sub[end_off + i * 2 + 1]]);
        let delta = i16::from_be_bytes([sub[delta_off + i * 2], sub[delta_off + i * 2 + 1]]);
        let ro = u16::from_be_bytes([sub[range_off + i * 2], sub[range_off + i * 2 + 1]]);
        if start > end {
            continue;
        }
        if ro != 0 {
            let base = range_off + i * 2 + ro as usize;
            if base + 2 > length {
                continue;
            }
            let idx = base - glyph_off;
            for c in start..=end {
                if c == 0xFFFF {
                    continue; // 非字符，必须映射到 .notdef（由终止段处理）
                }
                let p = idx + (c as usize - start as usize) * 2;
                if p + 2 > glyph_arr.len() {
                    break;
                }
                let g = u16::from_be_bytes([glyph_arr[p], glyph_arr[p + 1]]);
                if g != 0 {
                    mapping.entry(c).or_insert(g);
                }
            }
        } else {
            for c in start..=end {
                if c == 0xFFFF {
                    continue;
                }
                let g = ((c as i32 + delta as i32) & 0xFFFF) as u16;
                if g != 0 {
                    mapping.entry(c).or_insert(g);
                }
            }
        }
    }

    // 将连续码点且连续 glyph 的区间编码为一个段（idDelta 编码，idRangeOffset=0）
    let mut segs: Vec<(u16, u16, i16)> = Vec::new();
    {
        let mut iter = mapping.iter();
        if let Some((&c0, &g0)) = iter.next() {
            let mut cur_start = c0;
            let mut cur_g0 = g0 as i32;
            let mut prev_c = c0;
            let mut prev_g = g0 as i32;
            for (&c, &g) in iter {
                if c == prev_c + 1 && g as i32 == prev_g + 1 {
                    prev_c = c;
                    prev_g = g as i32;
                } else {
                    segs.push((cur_start, prev_c, (cur_g0 - cur_start as i32) as i16));
                    cur_start = c;
                    cur_g0 = g as i32;
                    prev_c = c;
                    prev_g = g as i32;
                }
            }
            segs.push((cur_start, prev_c, (cur_g0 - cur_start as i32) as i16));
        }
    }
    // 始终追加唯一的纯终止段
    segs.push((0xFFFF, 0xFFFF, 1));

    let nseg = segs.len();
    let seg_x2_out = (nseg * 2) as u16;
    let entry_sel = (nseg as u32).ilog2();
    let search_range = ((1u32 << entry_sel) as u16) * 2;
    let range_shift = seg_x2_out - search_range;

    let mut out = Vec::with_capacity(16 + nseg * 8);
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // length 占位，最后回填
    out.extend_from_slice(&0u16.to_be_bytes()); // language
    out.extend_from_slice(&seg_x2_out.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&(entry_sel as u16).to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());
    for &(_, e, _) in &segs {
        out.extend_from_slice(&e.to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    for &(s, _, _) in &segs {
        out.extend_from_slice(&s.to_be_bytes());
    }
    for &(_, _, d) in &segs {
        out.extend_from_slice(&d.to_be_bytes());
    }
    for _ in 0..nseg {
        out.extend_from_slice(&0u16.to_be_bytes());
    }
    let length_out = out.len() as u16;
    out[2..4].copy_from_slice(&length_out.to_be_bytes());
    Ok(out)
}

/// 重建 vmtx 表，使其长度覆盖全部字形
/// （OTS 要求表长度与 numberOfVMetrics / numGlyphs 匹配，否则报
/// "vmtx: Failed to read side bearing" / "vmtx: Failed to parse table"）。
/// 缺失的 side bearing 用最后一个已知值填充。
fn rebuild_vmtx_table(vmtx_raw: &[u8], nvm: usize, num_glyphs: usize) -> Vec<u8> {
    let total = nvm * 4 + (num_glyphs - nvm) * 2;
    let mut out = Vec::with_capacity(total);
    // 完整度量（advanceHeight + topSideBearing）
    let metrics_bytes = nvm * 4;
    let copy = vmtx_raw.len().min(metrics_bytes);
    out.extend_from_slice(&vmtx_raw[..copy]);
    out.resize(metrics_bytes, 0);
    // 剩余字形的 side bearing
    let sb = &vmtx_raw[metrics_bytes.min(vmtx_raw.len())..];
    let n_avail = sb.len() / 2;
    let mut last: i16 = 0;
    for i in 0..(num_glyphs - nvm) {
        let v = if i < n_avail {
            let val = i16::from_be_bytes([sb[i * 2], sb[i * 2 + 1]]);
            last = val;
            val
        } else {
            last
        };
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn convert_ftf(raw: &[u8]) -> Result<Vec<u8>> {
    convert_ftf_with(raw, &ConvertOptions::default())
}

/// 与 [`convert_ftf`] 相同，但可以额外请求彩色字体输出。
pub fn convert_ftf_with(raw: &[u8], opts: &ConvertOptions) -> Result<Vec<u8>> {
    if raw.len() < 12 {
        bail!("File too short to be a valid TTF");
    }
    let num_tables = u16::from_be_bytes([raw[4], raw[5]]) as usize;
    if raw.len() < 12 + num_tables * 16 {
        bail!("Corrupted table directory");
    }

    let mut orig_tables = HashMap::new();
    for i in 0..num_tables {
        let off = 12 + i * 16;
        let tag = &raw[off..off + 4];
        let toff = u32::from_be_bytes([raw[off + 8], raw[off + 9], raw[off + 10], raw[off + 11]]) as usize;
        let tlen = u32::from_be_bytes([raw[off + 12], raw[off + 13], raw[off + 14], raw[off + 15]]) as usize;
        if toff + tlen > raw.len() {
            bail!("Table {:?} points outside file boundary", std::str::from_utf8(tag));
        }
        orig_tables.insert(tag, &raw[toff..toff + tlen]);
    }

    // 如果没有 FTFH/FTFG 表，说明是正常 TTF，检查并修复空表问题
    if !orig_tables.contains_key(b"FTFH".as_slice()) || !orig_tables.contains_key(b"FTFG".as_slice()) {
        return fix_normal_ttf(raw);
    }

    let ftfh = orig_tables.get(b"FTFH".as_slice()).unwrap();
    let ftfg = orig_tables.get(b"FTFG".as_slice()).unwrap();
    let loca_raw = orig_tables.get(b"loca".as_slice()).ok_or_else(|| anyhow!("loca table missing"))?;
    let hhea_raw = orig_tables.get(b"hhea".as_slice()).ok_or_else(|| anyhow!("hhea table missing"))?;
    let hmtx_raw = orig_tables.get(b"hmtx".as_slice()).ok_or_else(|| anyhow!("hmtx table missing"))?;
    let head_raw = orig_tables.get(b"head".as_slice()).ok_or_else(|| anyhow!("head table missing"))?;
    let maxp_raw = orig_tables.get(b"maxp".as_slice()).ok_or_else(|| anyhow!("maxp table missing"))?;

    if ftfh.len() < 12 {
        bail!("FTFH header truncated");
    }
    let version = u32::from_be_bytes([ftfh[0], ftfh[1], ftfh[2], ftfh[3]]);
    let num_glyphs = u32::from_be_bytes([ftfh[4], ftfh[5], ftfh[6], ftfh[7]]) as usize;
    let flags_byte = ftfh[11];
    let mode1 = flags_byte >> 4;
    let mode2 = flags_byte & 0x0F;
    if version != 0x10000 {
        bail!("Unsupported FTFH version: 0x{:X}", version);
    }

    let mut loca_vals: Vec<usize> = loca_raw
        .chunks_exact(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as usize)
        .collect();

    // 处理loca表条目数与num_glyphs不匹配的情况
    if loca_vals.len() == num_glyphs {
        // loca表缺少最后一个条目，添加FTFG表的末尾作为最后一个条目
        loca_vals.push(ftfg.len());
    } else if loca_vals.len() > num_glyphs + 1 {
        // loca表条目数过多，截断到num_glyphs + 1
        loca_vals.truncate(num_glyphs + 1);
    } else if loca_vals.len() < num_glyphs {
        bail!(
            "loca entries ({}) is less than num_glyphs ({})",
            loca_vals.len(),
            num_glyphs
        );
    }

    let mut parser = FtfParser::new(ftfg, loca_vals, num_glyphs, mode1, mode2);
    let parsed_glyphs = parser.parse_all(num_glyphs)?;

    let mut glyf_bytes = Vec::new();
    let mut loca_bytes = Vec::with_capacity((num_glyphs + 1) * 4);
    let mut max_pts = 0usize;
    let mut max_contours = 0usize;
    let mut global_bbox: Option<(i16, i16, i16, i16)> = None;
    let mut glyph_bboxes = Vec::with_capacity(num_glyphs);

    for records in &parsed_glyphs {
        let off = glyf_bytes.len() as u32;
        loca_bytes.extend_from_slice(&off.to_be_bytes());

        let encoded = encode_simple_glyph(records);
        if encoded.pts_count > max_pts { max_pts = encoded.pts_count; }
        if encoded.contours_count > max_contours { max_contours = encoded.contours_count; }

        if let Some(b) = encoded.bbox {
            glyph_bboxes.push(Some(b));
            global_bbox = Some(match global_bbox {
                None => b,
                Some((x0, y0, x1, y1)) => (
                    x0.min(b.0),
                    y0.min(b.1),
                    x1.max(b.2),
                    y1.max(b.3),
                ),
            });
        } else {
            glyph_bboxes.push(None);
        }

        glyf_bytes.extend_from_slice(&encoded.data);
    }
    let final_off = glyf_bytes.len() as u32;
    loca_bytes.extend_from_slice(&final_off.to_be_bytes());

    let nhm = u16::from_be_bytes([hhea_raw[34], hhea_raw[35]]) as usize;
    let get_orig_hmtx = |gid: usize| -> (u16, i16) {
        if gid < nhm {
            let adv = u16::from_be_bytes([hmtx_raw[gid * 4], hmtx_raw[gid * 4 + 1]]);
            let lsb = i16::from_be_bytes([hmtx_raw[gid * 4 + 2], hmtx_raw[gid * 4 + 3]]);
            (adv, lsb)
        } else {
            let adv = u16::from_be_bytes([hmtx_raw[(nhm - 1) * 4], hmtx_raw[(nhm - 1) * 4 + 1]]);
            let rem = hmtx_raw.len().saturating_sub(nhm * 4);
            let off = gid - nhm;
            let lsb = if off < rem / 2 {
                i16::from_be_bytes([hmtx_raw[nhm * 4 + off * 2], hmtx_raw[nhm * 4 + off * 2 + 1]])
            } else {
                0
            };
            (adv, lsb)
        }
    };

    // 彩色字体：把 brsh（颜料）+ cglf（逐组选笔）编译成 COLR v1 / CPAL v0。
    // cglf 的组记录给定「哪些字形有色 + 用哪支笔」，解不出来时回退到全字形上色（详见 color.rs）。
    let color_tables = if opts.color {
        let paints = orig_tables
            .get(b"brsh".as_slice())
            .map(|b| color::parse_brsh(b))
            .unwrap_or_default();
        let cglf = orig_tables
            .get(b"cglf".as_slice())
            .and_then(|b| color::parse_cglf_brush(b, num_glyphs));
        // QQ 皮肤端的逐字色清单不在字体里，只能由调用方传入。
        let forced: Vec<u16> = match (&opts.color_chars, orig_tables.get(b"cmap".as_slice())) {
            (Some(chars), Some(raw)) => chars
                .iter()
                .filter_map(|c| cmap_lookup(raw, *c as u32))
                .collect(),
            _ => Vec::new(),
        };
        color::build_color_tables(&paints, cglf.as_deref(), &glyph_bboxes, num_glyphs, &forced)
    } else {
        None
    };

    let mut new_hmtx = Vec::with_capacity(num_glyphs * 4);
    for (i, bbox) in glyph_bboxes.iter().enumerate().take(num_glyphs) {
        let (adv, _) = get_orig_hmtx(i);
        let lsb = bbox.map(|b| b.0).unwrap_or(0);
        new_hmtx.extend_from_slice(&adv.to_be_bytes());
        new_hmtx.extend_from_slice(&lsb.to_be_bytes());
    }

    // 修复 vmtx/vhea：重建 vmtx 使其长度覆盖全部字形
    // （否则 OTS 报 "vmtx: Failed to read side bearing"），并保持 numberOfVMetrics 一致。
    let (new_vhea, new_vmtx) = match (
        orig_tables.get(b"vhea".as_slice()),
        orig_tables.get(b"vmtx".as_slice()),
    ) {
        (Some(vhea_raw), Some(vmtx_raw)) if vhea_raw.len() >= 36 && !vmtx_raw.is_empty() => {
            // OTS 要求 vhea 版本为 0x00010000（部分魔改字体写成 0x00010001 会报
            // "vhea: Unsupported table version"）
            let mut nvm = u16::from_be_bytes([vhea_raw[34], vhea_raw[35]]) as usize;
            let mut fixed_vhea = vhea_raw.to_vec();
            fixed_vhea[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
            if nvm > num_glyphs {
                nvm = num_glyphs;
                fixed_vhea[34..36].copy_from_slice(&(nvm as u16).to_be_bytes());
            }
            (Some(fixed_vhea), Some(rebuild_vmtx_table(vmtx_raw, nvm, num_glyphs)))
        }
        (Some(vhea_raw), _) if vhea_raw.len() >= 36 => {
            // 无 vmtx 时仍修复 vhea 版本号
            let mut fixed_vhea = vhea_raw.to_vec();
            fixed_vhea[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
            (Some(fixed_vhea), None)
        }
        _ => (None, None),
    };

    let mut new_hhea = hhea_raw.to_vec();
    // OTS 要求 hhea 版本为 0x00010000
    new_hhea[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
    let num_glyphs_u16 = num_glyphs as u16;
    new_hhea[34..36].copy_from_slice(&num_glyphs_u16.to_be_bytes());

    let mut new_head = head_raw.to_vec();
    // head表必须是54字节，如果原始FTF的head表更长则截断
    if new_head.len() > 54 {
        new_head.truncate(54);
    }
    let (g_xmin, g_ymin, g_xmax, g_ymax) = global_bbox.unwrap_or((0, 0, 0, 0));
    new_head[8..12].copy_from_slice(&[0, 0, 0, 0]);
    new_head[36..38].copy_from_slice(&g_xmin.to_be_bytes());
    new_head[38..40].copy_from_slice(&g_ymin.to_be_bytes());
    new_head[40..42].copy_from_slice(&g_xmax.to_be_bytes());
    new_head[42..44].copy_from_slice(&g_ymax.to_be_bytes());
    new_head[50..52].copy_from_slice(&1i16.to_be_bytes());
    new_head[52..54].copy_from_slice(&0i16.to_be_bytes());

    let mut new_maxp = maxp_raw.to_vec();
    if new_maxp.len() >= 32 {
        new_maxp[6..8].copy_from_slice(&(max_pts as u16).to_be_bytes());
        new_maxp[8..10].copy_from_slice(&(max_contours as u16).to_be_bytes());
        new_maxp[10..12].copy_from_slice(&0u16.to_be_bytes());
        new_maxp[12..14].copy_from_slice(&0u16.to_be_bytes());
        new_maxp[28..30].copy_from_slice(&0u16.to_be_bytes());
        new_maxp[30..32].copy_from_slice(&0u16.to_be_bytes());
    }

    let mut tables_map: BTreeMap<&[u8], Vec<u8>> = BTreeMap::new();
    for (tag, data) in &orig_tables {
        if *tag == b"FTFG" || *tag == b"FTFH" || *tag == b"glyf" || *tag == b"loca" {
            continue;
        }
        if color_tables.is_some() && is_private_color_table(tag) {
            continue;
        }
        if *tag == b"hmtx" {
            tables_map.insert(b"hmtx", new_hmtx.clone());
        } else if *tag == b"hhea" {
            tables_map.insert(b"hhea", new_hhea.clone());
        } else if *tag == b"head" {
            tables_map.insert(b"head", new_head.clone());
        } else if *tag == b"maxp" {
            tables_map.insert(b"maxp", new_maxp.clone());
        } else if *tag == b"cmap" {
            // 重建 cmap：修复 format 4 多个 0xFFFF 终止段（OTS 报错）
            tables_map.insert(b"cmap", rebuild_cmap_table(data)?);
        } else if *tag == b"gasp" {
            // OTS 要求 gasp 版本号为 1（否则报 "Changed the version number to 1"）
            if data.len() >= 4 {
                let mut fixed_gasp = data.to_vec();
                fixed_gasp[0..2].copy_from_slice(&1u16.to_be_bytes());
                tables_map.insert(b"gasp", fixed_gasp);
            } else if !data.is_empty() {
                tables_map.insert(tag, data.to_vec());
            }
        } else if *tag == b"vhea" {
            if let Some(v) = &new_vhea {
                tables_map.insert(b"vhea", v.clone());
            } else if !data.is_empty() {
                tables_map.insert(tag, data.to_vec());
            }
        } else if *tag == b"vmtx" {
            if let Some(v) = &new_vmtx {
                tables_map.insert(b"vmtx", v.clone());
            } else if !data.is_empty() {
                tables_map.insert(tag, data.to_vec());
            }
        } else if *tag == b"post" {
            // 修复 post 表：转换为 3.0 版本（无 glyph 名称），避免 numGlyphs 不匹配
            if data.len() >= 32 {
                let mut new_post = Vec::with_capacity(32);
                new_post.extend_from_slice(&0x00030000u32.to_be_bytes()); // version 3.0
                new_post.extend_from_slice(&data[4..32]); // 复制其余的基本字段
                tables_map.insert(b"post", new_post);
            } else if !data.is_empty() {
                tables_map.insert(tag, data.to_vec());
            }
        } else {
            // 跳过长度为 0 的表（OTS 不接受空表）
            if !data.is_empty() {
                tables_map.insert(tag, data.to_vec());
            }
        }
    }
    tables_map.insert(b"glyf", glyf_bytes);
    tables_map.insert(b"loca", loca_bytes);
    if let Some(ct) = &color_tables {
        tables_map.insert(b"COLR", ct.colr.clone());
        tables_map.insert(b"CPAL", ct.cpal.clone());
    }

    let out_num_tables = tables_map.len() as u16;
    let entry_selector = (out_num_tables as f64).log2().floor() as u16;
    let search_range = (1 << entry_selector) * 16;
    let range_shift = out_num_tables * 16 - search_range;

    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&out_num_tables.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());

    let header_size = 12 + out_num_tables as usize * 16;
    let mut current_offset = header_size;
    let mut dir_entries = Vec::new();
    let mut table_blobs = Vec::new();
    let mut head_table_file_offset = 0;

    for (tag, data) in tables_map {
        let checksum = calc_table_checksum(&data);
        let offset = current_offset as u32;
        let length = data.len() as u32;

        if tag == b"head" {
            head_table_file_offset = offset as usize;
        }

        dir_entries.push((tag, checksum, offset, length));
        table_blobs.push(data);

        let padded_len = (length as usize + 3) & !3;
        current_offset += padded_len;
    }

    for (tag, checksum, offset, length) in &dir_entries {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
    }

    for blob in table_blobs {
        out.extend_from_slice(&blob);
        let rem = blob.len() % 4;
        if rem != 0 {
            out.extend_from_slice(&vec![0u8; 4 - rem]);
        }
    }

    let whole_checksum = calc_table_checksum(&out);
    let check_sum_adj = 0xB1B0AFBA_u32.wrapping_sub(whole_checksum);
    if head_table_file_offset > 0 && head_table_file_offset + 12 <= out.len() {
        out[head_table_file_offset + 8..head_table_file_offset + 12]
            .copy_from_slice(&check_sum_adj.to_be_bytes());
    }

    Ok(out)
}
