use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

const X_OFF: i32 = 128;
const Y_OFF: i32 = 92;

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

    fn read_transform(&self, r: &[u8], pos: &mut usize) -> Result<[i32; 6]> {
        if *pos >= r.len() {
            bail!("Unexpected EOF reading transform flags");
        }
        let v6 = r[*pos];
        *pos += 1;
        let mut m = [64, 0, 0, 64, 0, 0];

        let specs = [
            (1, 0, self.mode1),
            (2, 1, self.mode1),
            (4, 2, self.mode1),
            (8, 3, self.mode1),
            (16, 4, self.mode2),
            (32, 5, self.mode2),
        ];

        for (bit, idx, mode) in specs {
            if (v6 & bit) != 0 {
                m[idx] = Self::read_coord(r, pos, mode)?;
            }
        }
        m[4] <<= 6;
        m[5] <<= 6;
        Ok(m)
    }

    #[inline(always)]
    fn apply_matrix(p: (i32, i32), m: &[i32; 6]) -> (i32, i32) {
        let (x, y) = p;
        (
            (m[0] * x + m[2] * y + m[4]) >> 6,
            (m[1] * x + m[3] * y + m[5]) >> 6,
        )
    }

    fn raw_outline(&mut self, gid: usize) -> Result<Vec<SubRecord>> {
        if gid >= self.loca_vals.len() - 1 {
            bail!("GID {} out of range", gid);
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
                    let m = self.read_transform(r, &mut pos)?;
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

// --------------------------- TTF Construction ---------------------------

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
    // instruction length = 0
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

    let loca_vals: Vec<usize> = loca_raw
        .chunks_exact(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as usize)
        .collect();

    if loca_vals.len() != num_glyphs + 1 {
        bail!(
            "loca entries ({}) mismatch with num_glyphs + 1 ({})",
            loca_vals.len(),
            num_glyphs + 1
        );
    }

    // 1. Parse outlines
    let mut parser = FtfParser::new(ftfg, loca_vals, num_glyphs, mode1, mode2);
    let parsed_glyphs = parser.parse_all(num_glyphs)?;

    // 2. Encode to Glyf & Loca
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

    // 3. Rebuild hmtx
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

    // 4. Update hhea
    let mut new_hhea = hhea_raw.to_vec();
    let num_glyphs_u16 = num_glyphs as u16;
    new_hhea[34..36].copy_from_slice(&num_glyphs_u16.to_be_bytes());

    // 5. Update head
    let mut new_head = head_raw.to_vec();
    let (g_xmin, g_ymin, g_xmax, g_ymax) = global_bbox.unwrap_or((0, 0, 0, 0));
    new_head[8..12].copy_from_slice(&[0, 0, 0, 0]); // checkSumAdjustment = 0
    new_head[36..38].copy_from_slice(&g_xmin.to_be_bytes());
    new_head[38..40].copy_from_slice(&g_ymin.to_be_bytes());
    new_head[40..42].copy_from_slice(&g_xmax.to_be_bytes());
    new_head[42..44].copy_from_slice(&g_ymax.to_be_bytes());
    new_head[50..52].copy_from_slice(&1i16.to_be_bytes()); // indexToLocFormat = 1 (long)

    // 6. Update maxp
    let mut new_maxp = maxp_raw.to_vec();
    if new_maxp.len() >= 32 {
        new_maxp[6..8].copy_from_slice(&(max_pts as u16).to_be_bytes());
        new_maxp[8..10].copy_from_slice(&(max_contours as u16).to_be_bytes());
        new_maxp[10..12].copy_from_slice(&0u16.to_be_bytes()); // maxCompositePoints
        new_maxp[12..14].copy_from_slice(&0u16.to_be_bytes()); // maxCompositeContours
        new_maxp[28..30].copy_from_slice(&0u16.to_be_bytes()); // maxComponentElements
        new_maxp[30..32].copy_from_slice(&0u16.to_be_bytes()); // maxComponentDepth
    }

    // 7. Collect remaining tables and write new TTF
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

    // 8. Fix head checkSumAdjustment
    let whole_checksum = calc_table_checksum(&out);
    let check_sum_adj = 0xB1B0AFBA_u32.wrapping_sub(whole_checksum);
    if head_table_file_offset > 0 && head_table_file_offset + 12 <= out.len() {
        out[head_table_file_offset + 8..head_table_file_offset + 12]
            .copy_from_slice(&check_sum_adj.to_be_bytes());
    }

    Ok(out)
}

fn process_single_file(src: &Path, dst: &Path) -> Result<()> {
    let raw = fs::read(src).with_context(|| format!("Failed to read {:?}", src))?;
    let converted = convert_ftf(&raw).with_context(|| format!("Failed to convert {:?}", src))?;
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
            let mut out = cli.input.clone();
            out.set_extension("ttf");
            out
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