//! Encrypted `.moe` translation containers.
//!
//! A `.moe` file is a block-aligned encrypted container holding fixed-size
//! text records. This module implements only the container and cipher boundary:
//! no key, no translation data, and no vendor secret is bundled here. A host
//! that is entitled to read a container supplies a [`MoeCipher`] at session
//! start, so supporting a format never requires redistributing its contents.
//!
//! The block geometry below is derived from observing a container on disk:
//! blocks are 16 bytes, the payload is the block-aligned prefix, and any
//! remainder is a short trailer. Repeated identical blocks in long runs are
//! the signature of ECB-style encryption over zero-filled record padding;
//! they say nothing about the key and are used here only for diagnostics.

use std::collections::HashMap;

use crate::nls::Nls;

/// Cipher block size of the observed containers.
pub const MOE_BLOCK_SIZE: usize = 16;

/// Errors from container parsing. Kept separate from [`crate::AssetError`] so
/// hosts can report a missing or declined cipher without an asset lookup
/// failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MoeError {
    /// Fewer bytes than one cipher block.
    TooShort { len: usize },
    /// The supplied cipher refused the payload.
    Cipher(&'static str),
    /// The schema does not fit the decrypted payload.
    Schema(&'static str),
    /// The LZ body stream is malformed or uses an unsupported variant.
    Lz(&'static str),
}

/// Supplied by the host. Implementations own any key material; this crate
/// never stores, derives, or ships one.
///
/// `data` is always a multiple of [`MOE_BLOCK_SIZE`] bytes and `first_block`
/// is the index of `data[0]` within the container, for ciphers that bind each
/// block to its position.
pub trait MoeCipher {
    fn decrypt_in_place(&self, data: &mut [u8], first_block: u64) -> Result<(), MoeError>;
}

/// Geometry of a container's ciphertext.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MoeLayout {
    pub block_size: usize,
    pub payload_len: usize,
    pub trailer_len: usize,
}

impl MoeLayout {
    pub fn of(len: usize) -> Self {
        let payload_len = len & !(MOE_BLOCK_SIZE - 1);
        Self {
            block_size: MOE_BLOCK_SIZE,
            payload_len,
            trailer_len: len - payload_len,
        }
    }
}

/// Describes how records are laid out after decryption. Supplied by the host
/// rather than hardcoded, because the record layout is a property of the
/// container the host chose to open, not of the engine.
#[derive(Clone, Copy, Debug)]
pub struct MoeSchema {
    /// Bytes before the first record.
    pub header_len: usize,
    /// Stride between records, including padding.
    pub record_size: usize,
    pub key_offset: usize,
    pub key_len: usize,
    pub text_offset: usize,
    pub text_len: usize,
    /// Encoding of both the key and the text fields.
    pub nls: Nls,
}

impl MoeSchema {
    fn field<'a>(&self, record: &'a [u8], offset: usize, len: usize) -> &'a [u8] {
        let end = offset.saturating_add(len).min(record.len());
        if offset >= end {
            return &[];
        }
        let bytes = &record[offset..end];
        let nul = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        &bytes[..nul]
    }
}

/// One text record after decryption.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoeRecord {
    pub key: String,
    pub text: String,
}

/// A decrypted container, ready to be read through a [`MoeSchema`].
#[derive(Clone, Debug)]
pub struct MoeArchive {
    payload: Vec<u8>,
    layout: MoeLayout,
    trailer: Vec<u8>,
}

impl MoeArchive {
    /// Decrypts the block-aligned payload. The trailer is preserved verbatim;
    /// it is not interpreted, since its meaning is not established.
    pub fn open(bytes: &[u8], cipher: &dyn MoeCipher) -> Result<Self, MoeError> {
        if bytes.len() < MOE_BLOCK_SIZE {
            return Err(MoeError::TooShort { len: bytes.len() });
        }
        let layout = MoeLayout::of(bytes.len());
        let mut payload = bytes[..layout.payload_len].to_vec();
        cipher.decrypt_in_place(&mut payload, 0)?;
        Ok(Self {
            payload,
            layout,
            trailer: bytes[layout.payload_len..].to_vec(),
        })
    }

    pub fn layout(&self) -> MoeLayout {
        self.layout
    }

    pub fn trailer(&self) -> &[u8] {
        &self.trailer
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Reads fixed-size records. Malformed encodings are skipped rather than
    /// failing the whole container, so one bad record cannot block a game boot.
    pub fn records(&self, schema: &MoeSchema) -> Result<Vec<MoeRecord>, MoeError> {
        if schema.record_size == 0 {
            return Err(MoeError::Schema("record_size must be nonzero"));
        }
        if schema.header_len > self.payload.len() {
            return Err(MoeError::Schema("header exceeds payload"));
        }
        if schema.key_offset.saturating_add(schema.key_len) > schema.record_size
            || schema.text_offset.saturating_add(schema.text_len) > schema.record_size
        {
            return Err(MoeError::Schema("field exceeds record stride"));
        }
        let mut out = Vec::new();
        let mut offset = schema.header_len;
        while offset + schema.record_size <= self.payload.len() {
            let record = &self.payload[offset..offset + schema.record_size];
            offset += schema.record_size;
            let key_bytes = schema.field(record, schema.key_offset, schema.key_len);
            let text_bytes = schema.field(record, schema.text_offset, schema.text_len);
            if key_bytes.is_empty() || text_bytes.is_empty() {
                continue;
            }
            let key = schema.nls.decode(key_bytes);
            let text = schema.nls.decode(text_bytes);
            if let (Ok(key), Ok(text)) = (key, text) {
                out.push(MoeRecord { key, text });
            }
        }
        Ok(out)
    }

    pub fn text_table(&self, schema: &MoeSchema) -> Result<MoeTextTable, MoeError> {
        Ok(MoeTextTable::from_records(self.records(schema)?))
    }
}

/// Lookup table for the text renderer. A miss leaves the original string
/// untouched, so a partial container still runs the game.
#[derive(Clone, Debug, Default)]
pub struct MoeTextTable {
    entries: HashMap<String, String>,
}

impl MoeTextTable {
    pub fn from_records(records: Vec<MoeRecord>) -> Self {
        let mut entries = HashMap::with_capacity(records.len());
        for record in records {
            entries.insert(record.key, record.text);
        }
        Self { entries }
    }

    pub fn lookup(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Structural diagnostics for a container, without attempting to decrypt it.
/// Useful for confirming that two files share a format or for locating record
/// padding regions while reverse engineering the layout.
#[derive(Clone, Debug)]
pub struct MoeProbe {
    pub len: usize,
    pub layout: MoeLayout,
    /// Shannon entropy in bits per byte over the first 2 MiB of the payload.
    pub entropy_bits_per_byte: f64,
    /// Distinct 16-byte blocks that occur more than once.
    pub repeated_blocks: usize,
    /// Copies beyond the first of those blocks.
    pub repeated_copies: usize,
    /// Runs of at least two consecutive identical blocks.
    pub constant_runs: usize,
    /// Bytes covered by those runs.
    pub constant_run_bytes: usize,
}

pub fn probe(bytes: &[u8]) -> MoeProbe {
    let layout = MoeLayout::of(bytes.len());
    let sample = &bytes[..layout.payload_len.min(2 * 1024 * 1024)];
    let mut histogram = [0u64; 256];
    for byte in sample {
        histogram[*byte as usize] += 1;
    }
    let total = sample.len() as f64;
    let entropy = histogram
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = *count as f64 / total;
            -p * p.log2()
        })
        .sum::<f64>();

    let mut counts: HashMap<[u8; MOE_BLOCK_SIZE], usize> = HashMap::new();
    let blocks = sample.len() / MOE_BLOCK_SIZE;
    for index in 0..blocks {
        let start = index * MOE_BLOCK_SIZE;
        let mut block = [0u8; MOE_BLOCK_SIZE];
        block.copy_from_slice(&sample[start..start + MOE_BLOCK_SIZE]);
        *counts.entry(block).or_insert(0) += 1;
    }
    let repeated: Vec<usize> = counts.values().copied().filter(|n| *n > 1).collect();

    let mut constant_runs = 0usize;
    let mut constant_run_bytes = 0usize;
    let mut index = 0usize;
    while index < blocks {
        let start = index * MOE_BLOCK_SIZE;
        let block = &sample[start..start + MOE_BLOCK_SIZE];
        let mut end = index + 1;
        while end < blocks && &sample[end * MOE_BLOCK_SIZE..(end + 1) * MOE_BLOCK_SIZE] == block {
            end += 1;
        }
        if end - index > 1 {
            constant_runs += 1;
            constant_run_bytes += (end - index) * MOE_BLOCK_SIZE;
        }
        index = end.max(index + 1);
    }

    MoeProbe {
        len: bytes.len(),
        layout,
        entropy_bits_per_byte: entropy,
        repeated_blocks: repeated.len(),
        repeated_copies: repeated.iter().sum::<usize>() - repeated.len(),
        constant_runs,
        constant_run_bytes,
    }
}

/// Magic of a decrypted `.moe` header.
pub const MOE_MAGIC: [u8; 4] = *b"XMOE";
/// Size of the encrypted file header, and of every entry's fixed prefix.
pub const MOE_HEADER_SIZE: usize = 24;
/// Code page used by the runtime's text path.
pub const MOE_TEXT_CODEPAGE: u16 = 936;

/// The three constant tables of the container cipher. A host supplies these
/// from its own copy of the patch; this crate intentionally ships no table
/// bytes, so supporting the format never redistributes the patch's data.
#[derive(Clone, Debug)]
pub struct MoeTables {
    /// 256-byte substitution table for the file header.
    pub sbox: Vec<u8>,
    /// 16-entry nibble table used by the index stream.
    pub nibble: Vec<u8>,
    /// Key dwords, indexed by `byte_index / 8` within a segment.
    pub keys: Vec<u32>,
}

impl MoeTables {
    pub fn validate(&self) -> Result<(), MoeError> {
        if self.sbox.len() < MOE_HEADER_SIZE {
            return Err(MoeError::Schema("sbox shorter than the 24-byte header"));
        }
        if self.nibble.len() < 16 {
            return Err(MoeError::Schema("nibble table shorter than 16 bytes"));
        }
        if self.keys.is_empty() {
            return Err(MoeError::Schema("no key dwords"));
        }
        Ok(())
    }
}

/// The decrypted 24-byte file header. Only the magic, the entry count and the
/// payload length have an established meaning; the last three words are kept
/// opaque until native evidence identifies them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MoeHeader {
    pub magic: [u8; 4],
    pub entry_count: u32,
    pub payload_len: u32,
    pub words: [u32; 3],
}

impl MoeHeader {
    pub fn decrypt(bytes: &[u8], tables: &MoeTables) -> Result<Self, MoeError> {
        tables.validate()?;
        if bytes.len() < MOE_HEADER_SIZE {
            return Err(MoeError::TooShort { len: bytes.len() });
        }
        let mut head = [0u8; MOE_HEADER_SIZE];
        for (index, slot) in head.iter_mut().enumerate() {
            *slot = bytes[index] ^ tables.sbox[index];
        }
        if head[..4] != MOE_MAGIC[..] {
            return Err(MoeError::Schema("bad XMOE magic"));
        }
        let word = |at: usize| {
            u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]])
        };
        Ok(Self {
            magic: MOE_MAGIC,
            entry_count: word(4),
            payload_len: word(8),
            words: [word(12), word(16), word(20)],
        })
    }
}

/// Cipher for the block-aligned index region. The runtime refreshes its key
/// state at segment boundaries; `segment_len` lets a host reproduce that.
/// Resource bodies are LZ-compressed after this layer and are not handled here.
pub struct XmoeCipher<'a> {
    tables: &'a MoeTables,
    /// Zero means one continuous stream (used by the tests and by callers that
    /// already know the index region fits in a single segment).
    pub segment_len: usize,
}

impl<'a> XmoeCipher<'a> {
    pub fn new(tables: &'a MoeTables) -> Self {
        Self { tables, segment_len: 0 }
    }

    fn stream_byte(&self, index: usize) -> u8 {
        let keys = &self.tables.keys;
        let key = keys[(index >> 3) % keys.len()];
        let nibble = ((key >> ((index & 7) * 4)) & 0xF) as usize;
        let high = ((key ^ 0x9896_EEA7) >> ((index & 3) * 8)) as u8;
        self.tables.nibble[nibble] ^ high
    }
}

impl<'a> MoeCipher for XmoeCipher<'a> {
    fn decrypt_in_place(&self, data: &mut [u8], first_block: u64) -> Result<(), MoeError> {
        self.tables.validate()?;
        let base = (first_block as usize).saturating_mul(MOE_BLOCK_SIZE);
        for (offset, byte) in data.iter_mut().enumerate() {
            let position = base + offset;
            let index = if self.segment_len > 0 {
                position % self.segment_len
            } else {
                position
            };
            *byte ^= self.stream_byte(index);
        }
        Ok(())
    }
}

/// One entry in the decrypted index region: a 24-byte fixed prefix followed by
/// a NUL-terminated name. The prefix carries the body length and type, whose
/// field offsets are not yet confirmed, so they are exposed as raw bytes.
#[derive(Clone, Copy, Debug)]
pub struct MoeEntry<'a> {
    pub fixed: &'a [u8],
    pub name: &'a [u8],
    pub body_offset: usize,
}

impl<'a> MoeEntry<'a> {
    /// Dispatch selector used by the runtime's two LZ decoders.
    pub fn kind(&self) -> u8 {
        self.fixed.first().map_or(0, |byte| (*byte >> 5) + 1)
    }
}

/// Reads one entry at `offset`. The caller advances using the length field in
/// `fixed` once its offset is confirmed.
pub fn read_entry<'a>(data: &'a [u8], offset: usize) -> Option<MoeEntry<'a>> {
    if offset + MOE_HEADER_SIZE > data.len() {
        return None;
    }
    let fixed = &data[offset..offset + MOE_HEADER_SIZE];
    let name_start = offset + MOE_HEADER_SIZE;
    let end = data[name_start..].iter().position(|byte| *byte == 0)? + name_start;
    Some(MoeEntry {
        fixed,
        name: &data[name_start..end],
        body_offset: end + 1,
    })
}

/// PJW/ELF hash used by the runtime to index entries by name.
pub fn name_hash(name: &[u8]) -> u32 {
    let mut hash = 0u32;
    for byte in name {
        hash = (hash << 4).wrapping_add(*byte as u32);
        let high = hash & 0xF000_0000;
        if high != 0 {
            hash ^= high >> 24;
        }
        hash &= !high;
    }
    hash
}

fn take(input: &[u8], pos: &mut usize) -> Result<u8, MoeError> {
    let byte = *input.get(*pos).ok_or(MoeError::Lz("truncated body stream"))?;
    *pos += 1;
    Ok(byte)
}

/// Decodes an entry body. The runtime has two LZ77/LZSS variants selected by
/// `(entry.fixed[0] >> 5) + 1`:
///
/// - control `< 0x20`: literal run of `control + 1` bytes;
/// - otherwise: `extra = (control >> 5) - 1`, a second byte extends the
///   back-reference distance, and the match length is `extra + 3`. A distance
///   of one repeats the last output byte (RLE).
///
/// Variant 2 extends `extra == 6` through a `0xFF` terminator chain, and a
/// `0xFF` distance byte with `control & 0x1F == 0x1F` switches to a 16-bit
/// big-endian distance biased by `0x1FFF`.
///
/// This is a reconstruction from static disassembly. It has not been checked
/// against a real container body, so callers should treat a decode failure as
/// a missing resource rather than a fatal engine error.
pub fn lz_decode(kind: u8, input: &[u8], expected_len: usize) -> Result<Vec<u8>, MoeError> {
    if kind != 1 && kind != 2 {
        return Err(MoeError::Lz("unsupported body codec"));
    }
    let mut out: Vec<u8> = Vec::with_capacity(expected_len);
    let mut pos = 0usize;

    while pos < input.len() && out.len() < expected_len {
        let control = take(input, &mut pos)?;
        if control < 0x20 {
            let count = control as usize + 1;
            if out.len() + count > expected_len || pos + count > input.len() {
                return Err(MoeError::Lz("literal run exceeds output or input"));
            }
            out.extend_from_slice(&input[pos..pos + count]);
            pos += count;
            continue;
        }

        let mut extra = (control >> 5) as usize - 1;
        if extra == 6 {
            if kind == 1 {
                extra = take(input, &mut pos)? as usize + 6;
            } else {
                extra = 6;
                loop {
                    let byte = take(input, &mut pos)?;
                    extra += byte as usize;
                    if byte != 0xFF {
                        break;
                    }
                }
            }
        }

        let low = take(input, &mut pos)?;
        let mut distance = ((control & 0x1F) as usize) << 8 | low as usize;
        distance += 1;
        if kind == 2 && low == 0xFF && (control & 0x1F) == 0x1F {
            let high = take(input, &mut pos)? as usize;
            let low2 = take(input, &mut pos)? as usize;
            distance = (high << 8 | low2) + 0x1FFF + 1;
        }

        let total = extra + 3;
        if out.len() + total > expected_len {
            return Err(MoeError::Lz("match exceeds expected output length"));
        }
        if distance == 1 {
            let byte = *out.last().ok_or(MoeError::Lz("RLE without history"))?;
            out.resize(out.len() + total, byte);
        } else {
            if distance > out.len() {
                return Err(MoeError::Lz("back-reference before output start"));
            }
            for _ in 0..total {
                let byte = out[out.len() - distance];
                out.push(byte);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only toy cipher. It is deliberately unrelated to any shipped
    /// container and exists only to exercise the container boundary.
    struct XorCipher(u8);

    impl MoeCipher for XorCipher {
        fn decrypt_in_place(&self, data: &mut [u8], _first_block: u64) -> Result<(), MoeError> {
            for byte in data.iter_mut() {
                *byte ^= self.0;
            }
            Ok(())
        }
    }

    struct RefusingCipher;

    impl MoeCipher for RefusingCipher {
        fn decrypt_in_place(&self, _: &mut [u8], _: u64) -> Result<(), MoeError> {
            Err(MoeError::Cipher("no key configured"))
        }
    }

    fn schema() -> MoeSchema {
        MoeSchema {
            header_len: 0,
            record_size: 32,
            key_offset: 0,
            key_len: 8,
            text_offset: 8,
            text_len: 16,
            nls: Nls::Utf8,
        }
    }

    fn encrypted_container() -> Vec<u8> {
        let mut plain = vec![0u8; 64];
        plain[..4].copy_from_slice(b"key1");
        plain[8..15].copy_from_slice(b"first  ");
        plain[32..36].copy_from_slice(b"key2");
        plain[40..46].copy_from_slice(b"second");
        plain.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        plain.iter_mut().take(64).for_each(|b| *b ^= 0x5A);
        plain
    }

    #[test]
    fn layout_keeps_the_unaligned_trailer() {
        let layout = MoeLayout::of(68);
        assert_eq!(layout.block_size, 16);
        assert_eq!(layout.payload_len, 64);
        assert_eq!(layout.trailer_len, 4);
    }

    #[test]
    fn open_decrypts_payload_and_preserves_trailer() {
        let bytes = encrypted_container();
        let archive = MoeArchive::open(&bytes, &XorCipher(0x5A)).unwrap();
        assert_eq!(archive.trailer(), &[0xAA, 0xBB, 0xCC, 0xDD][..]);
        assert_eq!(&archive.payload()[..4], &b"key1"[..]);
        assert_eq!(archive.layout().payload_len, 64);
    }

    #[test]
    fn a_declined_cipher_is_reported_before_any_parse() {
        let err = MoeArchive::open(&encrypted_container(), &RefusingCipher).unwrap_err();
        assert_eq!(err, MoeError::Cipher("no key configured"));
    }

    #[test]
    fn records_and_lookup_read_fixed_stride_records() {
        let archive = MoeArchive::open(&encrypted_container(), &XorCipher(0x5A)).unwrap();
        let records = archive.records(&schema()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].key, "key1");
        assert_eq!(records[0].text, "first  ");
        let table = archive.text_table(&schema()).unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(table.lookup("key2"), Some("second"));
        assert_eq!(table.lookup("missing"), None);
    }

    #[test]
    fn probe_detects_ecb_style_padding_runs() {
        let mut bytes = vec![0u8; 16 * 12];
        for i in 0..8 {
            bytes[i * 16..(i + 1) * 16].copy_from_slice(&[0x11; 16]);
        }
        for i in 8..12 {
            bytes[i * 16..(i + 1) * 16].copy_from_slice(&[(i as u8); 16]);
        }
        let report = probe(&bytes);
        assert_eq!(report.layout.payload_len, 16 * 12);
        assert_eq!(report.repeated_blocks, 1);
        assert_eq!(report.constant_runs, 1);
        assert_eq!(report.constant_run_bytes, 8 * 16);
    }

    #[test]
    fn short_input_is_rejected() {
        assert_eq!(
            MoeArchive::open(&[0u8; 4], &XorCipher(0)).unwrap_err(),
            MoeError::TooShort { len: 4 }
        );
    }

    /// Synthetic tables. No byte here comes from a shipped patch.
    fn test_tables() -> MoeTables {
        MoeTables {
            sbox: (0..=255u8).collect(),
            nibble: (0..16u8).collect(),
            keys: vec![0x1122_3344, 0x5566_7788],
        }
    }

    #[test]
    fn xmoe_header_decrypts_and_checks_magic() {
        let tables = test_tables();
        let mut plain = [0u8; MOE_HEADER_SIZE];
        plain[..4].copy_from_slice(&MOE_MAGIC);
        plain[4..8].copy_from_slice(&141u32.to_le_bytes());
        plain[8..12].copy_from_slice(&38_689_815u32.to_le_bytes());
        let encrypted: Vec<u8> = plain
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ tables.sbox[index])
            .collect();
        let header = MoeHeader::decrypt(&encrypted, &tables).unwrap();
        assert_eq!(header.magic, MOE_MAGIC);
        assert_eq!(header.entry_count, 141);
        assert_eq!(header.payload_len, 38_689_815);
        let mut broken = encrypted.clone();
        broken[0] ^= 1;
        assert_eq!(
            MoeHeader::decrypt(&broken, &tables).unwrap_err(),
            MoeError::Schema("bad XMOE magic")
        );
    }

    #[test]
    fn index_stream_is_reversible() {
        let tables = test_tables();
        let cipher = XmoeCipher::new(&tables);
        let original: Vec<u8> = (0..64u8).collect();
        let mut data = original.clone();
        cipher.decrypt_in_place(&mut data, 0).unwrap();
        assert_ne!(data, original);
        cipher.decrypt_in_place(&mut data, 0).unwrap();
        assert_eq!(data, original);
    }

    #[test]
    fn segment_length_restarts_the_stream() {
        let tables = test_tables();
        let mut cipher = XmoeCipher::new(&tables);
        cipher.segment_len = MOE_BLOCK_SIZE;
        let mut first = vec![0u8; MOE_BLOCK_SIZE];
        let mut second = vec![0u8; MOE_BLOCK_SIZE];
        cipher.decrypt_in_place(&mut first, 0).unwrap();
        cipher.decrypt_in_place(&mut second, 1).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn entries_are_a_fixed_header_plus_a_name() {
        let mut data = vec![0u8; MOE_HEADER_SIZE];
        data[0] = 0x20;
        data.extend_from_slice(b"scene01\0");
        data.extend_from_slice(&[0xAA, 0xBB]);
        let entry = read_entry(&data, 0).unwrap();
        assert_eq!(entry.name, &b"scene01"[..]);
        assert_eq!(entry.body_offset, MOE_HEADER_SIZE + 8);
        assert_eq!(entry.kind(), 2);
        assert!(read_entry(&data, data.len()).is_none());
    }

    #[test]
    fn name_hash_matches_pjw() {
        assert_eq!(name_hash(b""), 0);
        assert_eq!(name_hash(b"a"), 97);
    }

    #[test]
    fn lz_decodes_a_literal_run() {
        assert_eq!(
            lz_decode(1, &[0x02, b'a', b'b', b'c'], 3).unwrap(),
            &b"abc"[..]
        );
    }

    #[test]
    fn lz_decodes_an_overlapping_back_reference() {
        let input = [0x02, b'a', b'b', b'c', 0x20, 0x02];
        assert_eq!(lz_decode(1, &input, 6).unwrap().as_slice(), &b"abcabc"[..]);
    }

    #[test]
    fn lz_distance_one_repeats_the_last_byte() {
        let input = [0x00, b'x', 0x20, 0x00];
        assert_eq!(lz_decode(1, &input, 4).unwrap().as_slice(), &b"xxxx"[..]);
    }

    #[test]
    fn lz_variant_two_extends_the_match_length() {
        // 0xE0 sets extra to 6, which variant 2 extends through a chain;
        // the 0x02 byte terminates it and adds 2, giving an 11-byte match.
        let input = [0x02, b'a', b'b', b'c', 0xE0, 0x02, 0x02];
        let out = lz_decode(2, &input, 14).unwrap();
        assert_eq!(out.len(), 14);
        assert_eq!(&out[..6], &b"abcabc"[..]);
    }

    #[test]
    fn lz_rejects_unknown_variants_and_truncated_streams() {
        assert_eq!(
            lz_decode(3, &[], 0).unwrap_err(),
            MoeError::Lz("unsupported body codec")
        );
        assert!(lz_decode(1, &[0x20], 3).is_err());
        assert!(lz_decode(1, &[0x20, 0x00], 3).is_err());
    }
}
