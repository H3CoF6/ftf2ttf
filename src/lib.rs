use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, HashMap};

const X_OFF: i32 = 128;
const Y_OFF: i32 = 92;

#[derive(Debug, Clone)]
struct SubRecord {
    points: Vec<(i32, i32)>,
    flags: Vec<u8>,
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

                    if c1 > 0 {
                        pos += c2;
                    }
                    out.push(SubRecord { points: pts, flags });
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
                    for rec in child_records {
                        let transformed_pts = rec
                            .points
                            .into_iter()
                            .map(|p| Self::apply_matrix(p, &m))
                            .collect();
                        out.push(SubRecord {
                            points: transformed_pts,
                            flags: rec.flags,
                        });
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
            let mapped = recs
                .into_iter()
                .map(|rec| SubRecord {
                    points: rec
                        .points
                        .into_iter()
                        .map(|(x, y)| (x + X_OFF, Y_OFF - y))
                        .collect(),
                    flags: rec.flags,
                })
                .collect();
            glyphs.push(mapped);
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

pub fn convert_ftf(raw: &[u8]) -> Result<Vec<u8>> {
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

    let ftfh = orig_tables.get(b"FTFH".as_slice()).ok_or_else(|| anyhow!("FTFH table missing"))?;
    let ftfg = orig_tables.get(b"FTFG".as_slice()).ok_or_else(|| anyhow!("FTFG table missing"))?;
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

    // 有些FTF文件的loca表缺少最后一个条目，需要自动添加
    if loca_vals.len() == num_glyphs {
        // loca表缺少最后一个条目，添加FTFG表的末尾作为最后一个条目
        loca_vals.push(ftfg.len());
    } else if loca_vals.len() != num_glyphs + 1 {
        bail!(
            "loca entries ({}) mismatch with num_glyphs or num_glyphs + 1 (expected {} or {})",
            loca_vals.len(),
            num_glyphs,
            num_glyphs + 1
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

    let mut new_hmtx = Vec::with_capacity(num_glyphs * 4);
    for (i, bbox) in glyph_bboxes.iter().enumerate().take(num_glyphs) {
        let (adv, _) = get_orig_hmtx(i);
        let lsb = bbox.map(|b| b.0).unwrap_or(0);
        new_hmtx.extend_from_slice(&adv.to_be_bytes());
        new_hmtx.extend_from_slice(&lsb.to_be_bytes());
    }

    let mut new_hhea = hhea_raw.to_vec();
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
        if *tag == b"hmtx" {
            tables_map.insert(b"hmtx", new_hmtx.clone());
        } else if *tag == b"hhea" {
            tables_map.insert(b"hhea", new_hhea.clone());
        } else if *tag == b"head" {
            tables_map.insert(b"head", new_head.clone());
        } else if *tag == b"maxp" {
            tables_map.insert(b"maxp", new_maxp.clone());
        } else {
            tables_map.insert(tag, data.to_vec());
        }
    }
    tables_map.insert(b"glyf", glyf_bytes);
    tables_map.insert(b"loca", loca_bytes);

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
