// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! ACIS data of R2013+ drawings from the AcDsPrototype_1b section.
//!
//! Since AutoCAD 2013, solids store their SAB stream in this data-storage
//! section instead of the entity. LibreDWG decompresses the section but does
//! not attach the streams, so they are matched to entity handles here.
//! Layout as observed in AutoCAD- and ODA-written files:
//!
//! * The section is a list of segments, each with a 48-byte header
//!   (`AC D5`, 6-char name, RL segment index, ...).
//! * `_data_` segments start with index records `RL 20, RL 1, RLL handle,
//!   RL offset`, then 0x62 padding, then the data area. At `offset` there is
//!   either `RL size` + the stream, or a blob reference for big streams:
//!   `RL 0xBB106BB1, RLL total, RL pages, RL ref size, ...,
//!   (RL segment index, RL page size) x pages`.
//! * `blob01` segments hold those pages: header `RLL total, RLL offset,
//!   RL page, RL pages, RLL size`, then the page data.

use std::collections::HashMap;

const SEGMENT_NAMES: [&[u8; 6]; 9] = [
    b"segidx", b"datidx", b"_data_", b"schidx", b"schdat", b"search", b"blob01", b"prvsav",
    b"freesp",
];
const BLOB_REF: u32 = 0xBB10_6BB1;
const HEADER: usize = 48;

fn u32_at(data: &[u8], pos: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(pos..pos + 4)?.try_into().unwrap(),
    ))
}

fn u64_at(data: &[u8], pos: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(pos..pos + 8)?.try_into().unwrap(),
    ))
}

struct Segment<'a> {
    start: usize,
    end: usize,
    name: &'a [u8],
    index: u32,
}

/// Map entity handle -> stored data (the SAB stream for solids).
pub fn acis_streams(section: &[u8]) -> HashMap<u64, Vec<u8>> {
    let mut starts: Vec<(usize, &[u8])> = vec![];
    let mut pos = 0;
    while pos + 8 <= section.len() {
        let name = &section[pos + 2..pos + 8];
        if section[pos] == 0xAC
            && section[pos + 1] == 0xD5
            && SEGMENT_NAMES.iter().any(|n| n.as_slice() == name)
        {
            starts.push((pos, name));
            pos += 8;
        } else {
            pos += 1;
        }
    }
    let segments: Vec<Segment> = starts
        .iter()
        .enumerate()
        .filter_map(|(k, &(start, name))| {
            let end = starts.get(k + 1).map_or(section.len(), |s| s.0);
            Some(Segment {
                start,
                end,
                name,
                index: u32_at(section, start + 8)?,
            })
        })
        .collect();
    let blobs: HashMap<u32, (usize, usize)> = segments
        .iter()
        .filter(|s| s.name == b"blob01")
        .map(|s| (s.index, (s.start, s.end)))
        .collect();

    let mut out = HashMap::new();
    for seg in segments.iter().filter(|s| s.name == b"_data_") {
        let (mut pos, end) = (seg.start + HEADER, seg.end);
        let mut records = vec![];
        while pos + 20 <= end {
            let (Some(size), Some(one), Some(handle), Some(offset)) = (
                u32_at(section, pos),
                u32_at(section, pos + 4),
                u64_at(section, pos + 8),
                u32_at(section, pos + 16),
            ) else {
                break;
            };
            if size != 20 || one != 1 {
                break;
            }
            records.push((handle, offset as usize));
            pos += 20;
        }
        while pos < end && section[pos] == 0x62 {
            pos += 1; // padding
        }
        for (handle, offset) in records {
            if let Some(data) = entry(section, pos + offset, end, &blobs).filter(|d| !d.is_empty())
            {
                out.insert(handle, data);
            }
        }
    }
    out
}

fn entry(
    section: &[u8],
    pos: usize,
    end: usize,
    blobs: &HashMap<u32, (usize, usize)>,
) -> Option<Vec<u8>> {
    if pos + 8 > section.len() {
        return None;
    }
    let head = u32_at(section, pos)?;
    if head != BLOB_REF {
        let size = head as usize;
        return (size > 0 && size + 4 <= end.saturating_sub(pos))
            .then(|| section[pos + 4..pos + 4 + size].to_vec());
    }
    let total = u64_at(section, pos + 4)? as usize;
    let pages = u32_at(section, pos + 12)? as usize;
    let ref_size = u32_at(section, pos + 16)? as usize;
    let ref_end = pos + 4 + ref_size;
    let mut data = Vec::with_capacity(total.min(section.len()));
    for k in 0..pages {
        let at = ref_end.checked_sub(8 * (pages - k))?;
        let (seg, size) = (u32_at(section, at)?, u32_at(section, at + 4)? as usize);
        let &(s, e) = blobs.get(&seg)?;
        // blob01 page header: RLL total, RLL offset, RL page, RL pages, RLL size
        let page_size = u64_at(section, s + HEADER + 24)? as usize;
        let data_start = s + HEADER + 32;
        let len = page_size.min(size).min(e.saturating_sub(data_start));
        data.extend_from_slice(section.get(data_start..data_start + len)?);
    }
    (data.len() == total).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(name: &[u8; 6], index: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0xAC, 0xD5];
        out.extend_from_slice(name);
        for v in [index, 0, 0, 0, 1, 0, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[b'U'; 8]);
        out.truncate(HEADER);
        out.extend_from_slice(payload);
        out
    }

    fn le(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn inline_and_paged_streams() {
        let small = [b"ACIS BinaryFile".as_slice(), &[b'x'; 10]].concat();
        let big: Vec<u8> = [
            b"ASM BinaryFile4".as_slice(),
            &(0..=255u8).cycle().take(768).collect::<Vec<u8>>(),
        ]
        .concat();
        let (page1, page2) = big.split_at(400);
        // magic, total, pages, size of the rest (16 unknown bytes + page list)
        let blob_ref = le(&[
            &BLOB_REF.to_le_bytes(),
            &(big.len() as u64).to_le_bytes(),
            &2u32.to_le_bytes(),
            &(8u32 + 4 + 4 + 16 + 16).to_le_bytes(),
            &[0; 16],
            &3u32.to_le_bytes(),
            &(page1.len() as u32).to_le_bytes(),
            &4u32.to_le_bytes(),
            &(page2.len() as u32).to_le_bytes(),
        ]);
        let record = |handle: u64, offset: u32| {
            le(&[
                &20u32.to_le_bytes(),
                &1u32.to_le_bytes(),
                &handle.to_le_bytes(),
                &offset.to_le_bytes(),
            ])
        };
        let data = le(&[
            &record(0x2A, 0),
            &record(0x2B, 4 + small.len() as u32),
            b"bbbb",
            &(small.len() as u32).to_le_bytes(),
            &small,
            &blob_ref,
        ]);
        let mut section = vec![0u8; 128];
        section.extend(segment(b"_data_", 2, &data));
        for (idx, page, offset) in [(3u32, page1, 0usize), (4, page2, page1.len())] {
            let head = le(&[
                &(big.len() as u64).to_le_bytes(),
                &(offset as u64).to_le_bytes(),
                &(idx - 3).to_le_bytes(),
                &2u32.to_le_bytes(),
                &(page.len() as u64).to_le_bytes(),
            ]);
            section.extend(segment(b"blob01", idx, &[head.as_slice(), page].concat()));
        }
        let streams = acis_streams(&section);
        assert_eq!(streams[&0x2A], small);
        assert_eq!(streams[&0x2B], big);
    }
}
