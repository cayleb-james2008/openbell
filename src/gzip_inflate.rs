//! Minimal pure-Rust RFC 1952/1951 gzip inflator for serving embedded pre-gzipped UI assets
//! to non-gzip HTTP clients without storing duplicate uncompressed bytes in `.rodata`.

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u64,
    bitcnt: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], start: usize) -> Self {
        Self { data, pos: start, bitbuf: 0, bitcnt: 0 }
    }

    fn fill(&mut self, need: u8) -> Option<()> {
        while self.bitcnt < need {
            if self.pos >= self.data.len() {
                return None;
            }
            self.bitbuf |= (self.data[self.pos] as u64) << self.bitcnt;
            self.pos += 1;
            self.bitcnt += 8;
        }
        Some(())
    }

    fn read_bits(&mut self, n: u8) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        self.fill(n)?;
        let val = (self.bitbuf & ((1u64 << n) - 1)) as u32;
        self.bitbuf >>= n;
        self.bitcnt -= n;
        Some(val)
    }

    fn align_byte(&mut self) {
        let drop = self.bitcnt & 7;
        self.bitbuf >>= drop;
        self.bitcnt -= drop;
    }

    fn read_u16_aligned(&mut self) -> Option<u16> {
        self.align_byte();
        let lo = self.read_bits(8)? as u16;
        let hi = self.read_bits(8)? as u16;
        Some(lo | (hi << 8))
    }
}

struct HuffTable {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl HuffTable {
    fn from_lengths(lengths: &[u8]) -> Option<Self> {
        let mut counts = [0u16; 16];
        for &len in lengths {
            if len as usize > 15 {
                return None;
            }
            if len > 0 {
                counts[len as usize] += 1;
            }
        }
        let mut offs = [0u16; 16];
        let mut total = 0u16;
        for bits in 1..=15 {
            offs[bits] = total;
            total = total.checked_add(counts[bits])?;
        }
        let mut symbols = vec![0u16; total as usize];
        for (sym, &len) in lengths.iter().enumerate() {
            if len > 0 {
                let idx = offs[len as usize] as usize;
                offs[len as usize] += 1;
                symbols[idx] = sym as u16;
            }
        }
        Some(Self { counts, symbols })
    }

    fn decode(&self, br: &mut BitReader<'_>) -> Option<u16> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=15 {
            code |= br.read_bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return self.symbols.get((index + (code - first)) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
    131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    13, 13,
];
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn skip_cstr(data: &[u8], pos: &mut usize) -> Option<()> {
    while *pos < data.len() {
        let b = data[*pos];
        *pos += 1;
        if b == 0 {
            return Some(());
        }
    }
    None
}

/// Inflate a single-member gzip stream (RFC 1952 + RFC 1951).
pub(crate) fn inflate_gzip(gz: &[u8]) -> Option<Vec<u8>> {
    if gz.len() < 18 || gz[0] != 0x1f || gz[1] != 0x8b || gz[2] != 0x08 {
        return None;
    }
    let flg = gz[3];
    let mut pos = 10usize;
    if flg & 0x04 != 0 {
        if pos + 2 > gz.len() {
            return None;
        }
        let xlen = u16::from_le_bytes([gz[pos], gz[pos + 1]]) as usize;
        pos = pos.checked_add(2 + xlen)?;
    }
    if flg & 0x08 != 0 {
        skip_cstr(gz, &mut pos)?;
    }
    if flg & 0x10 != 0 {
        skip_cstr(gz, &mut pos)?;
    }
    if flg & 0x02 != 0 {
        pos = pos.checked_add(2)?;
    }
    let footer_start = gz.len().checked_sub(8)?;
    if pos > footer_start {
        return None;
    }

    let expected_len = u32::from_le_bytes([
        gz[gz.len() - 4],
        gz[gz.len() - 3],
        gz[gz.len() - 2],
        gz[gz.len() - 1],
    ]);
    let mut out = Vec::with_capacity((expected_len as usize).min(4 * 1024 * 1024));
    let mut br = BitReader::new(&gz[..footer_start], pos);

    loop {
        let bfinal = br.read_bits(1)?;
        let btype = br.read_bits(2)?;
        match btype {
            0 => {
                let len = br.read_u16_aligned()?;
                let nlen = br.read_u16_aligned()?;
                if len ^ nlen != 0xffff {
                    return None;
                }
                for _ in 0..len {
                    out.push(br.read_bits(8)? as u8);
                }
            }
            1 | 2 => {
                let (lit_tbl, dist_tbl) = if btype == 1 {
                    let mut lit_lens = [0u8; 288];
                    lit_lens[0..=143].fill(8);
                    lit_lens[144..=255].fill(9);
                    lit_lens[256..=279].fill(7);
                    lit_lens[280..=287].fill(8);
                    let dist_lens = [5u8; 32];
                    (HuffTable::from_lengths(&lit_lens)?, HuffTable::from_lengths(&dist_lens)?)
                } else {
                    let hlit = br.read_bits(5)? as usize + 257;
                    let hdist = br.read_bits(5)? as usize + 1;
                    let hclen = br.read_bits(4)? as usize + 4;
                    let mut cl_lens = [0u8; 19];
                    for i in 0..hclen {
                        cl_lens[CL_ORDER[i]] = br.read_bits(3)? as u8;
                    }
                    let cl_tbl = HuffTable::from_lengths(&cl_lens)?;
                    let total = hlit + hdist;
                    let mut lens = Vec::with_capacity(total);
                    while lens.len() < total {
                        let sym = cl_tbl.decode(&mut br)? as usize;
                        match sym {
                            0..=15 => lens.push(sym as u8),
                            16 => {
                                let prev = *lens.last()?;
                                let rep = br.read_bits(2)? as usize + 3;
                                lens.resize(lens.len() + rep, prev);
                            }
                            17 => {
                                let rep = br.read_bits(3)? as usize + 3;
                                lens.resize(lens.len() + rep, 0);
                            }
                            18 => {
                                let rep = br.read_bits(7)? as usize + 11;
                                lens.resize(lens.len() + rep, 0);
                            }
                            _ => return None,
                        }
                    }
                    if lens.len() != total {
                        return None;
                    }
                    (
                        HuffTable::from_lengths(&lens[..hlit])?,
                        HuffTable::from_lengths(&lens[hlit..])?,
                    )
                };

                loop {
                    let sym = lit_tbl.decode(&mut br)? as usize;
                    if sym < 256 {
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let li = sym.checked_sub(257)?;
                        let len = *LEN_BASE.get(li)? as usize + br.read_bits(*LEN_EXTRA.get(li)?)? as usize;
                        let di = dist_tbl.decode(&mut br)? as usize;
                        let dist = *DIST_BASE.get(di)? as usize + br.read_bits(*DIST_EXTRA.get(di)?)? as usize;
                        if dist == 0 || dist > out.len() {
                            return None;
                        }
                        for _ in 0..len {
                            let b = out[out.len() - dist];
                            out.push(b);
                        }
                    }
                }
            }
            _ => return None,
        }
        if bfinal == 1 {
            break;
        }
    }

    if (out.len() as u32) != expected_len {
        return None;
    }
    Some(out)
}
