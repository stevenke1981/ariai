use anyhow::{Result, bail, ensure};
use std::io::{Cursor, Read};

/// HTTP control files have no URL or validator; callers must verify the source independently.
pub struct Control {
    pub total: u64,
    pub piece_length: u32,
    pub complete: Vec<bool>,
    pub partial: Vec<Vec<bool>>,
}

impl Control {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        fn number<const N: usize>(c: &mut Cursor<&[u8]>) -> Result<[u8; N]> {
            let mut b = [0; N];
            c.read_exact(&mut b)?;
            Ok(b)
        }
        let mut c = Cursor::new(bytes);
        ensure!(
            u16::from_be_bytes(number(&mut c)?) == 1,
            "unsupported aria2 control version"
        );
        let extension = u32::from_be_bytes(number(&mut c)?);
        let hash_len = u32::from_be_bytes(number(&mut c)?);
        ensure!(
            extension == 0 && hash_len == 0,
            "not an HTTP aria2 control file"
        );
        let piece_length = u32::from_be_bytes(number(&mut c)?);
        let total = u64::from_be_bytes(number(&mut c)?);
        let _uploaded = u64::from_be_bytes(number(&mut c)?);
        ensure!(
            piece_length > 0 && total > 0,
            "invalid aria2 piece dimensions"
        );
        let count = total.div_ceil(u64::from(piece_length));
        ensure!(count <= 65_536, "aria2 control has too many pieces");
        let field_len = u32::from_be_bytes(number(&mut c)?) as usize;
        ensure!(
            field_len == (count as usize).div_ceil(8),
            "invalid aria2 bitfield length"
        );
        let mut field = vec![0; field_len];
        c.read_exact(&mut field)?;
        if count % 8 != 0 {
            ensure!(
                field[field_len - 1] & ((1 << (8 - count % 8)) - 1) == 0,
                "invalid aria2 bitfield padding"
            );
        }
        let complete: Vec<_> = (0..count as usize)
            .map(|i| field[i / 8] & (0x80 >> (i % 8)) != 0)
            .collect();
        let in_flight = u32::from_be_bytes(number(&mut c)?) as u64;
        ensure!(in_flight <= count, "invalid aria2 in-flight count");
        let mut seen = vec![false; count as usize];
        let mut partial = vec![Vec::new(); count as usize];
        for _ in 0..in_flight {
            let index = u32::from_be_bytes(number(&mut c)?) as usize;
            let size = u32::from_be_bytes(number(&mut c)?);
            let bits = u32::from_be_bytes(number(&mut c)?) as usize;
            ensure!(
                index < complete.len() && !seen[index] && !complete[index],
                "invalid aria2 in-flight index"
            );
            seen[index] = true;
            let actual =
                (total - index as u64 * u64::from(piece_length)).min(u64::from(piece_length));
            ensure!(
                u64::from(size) == actual && bits == (actual.div_ceil(16384) as usize).div_ceil(8),
                "invalid aria2 in-flight size"
            );
            let mut field = vec![0; bits];
            c.read_exact(&mut field)?;
            let chunks = actual.div_ceil(16384) as usize;
            if chunks % 8 != 0 {
                ensure!(
                    field[bits - 1] & ((1 << (8 - chunks % 8)) - 1) == 0,
                    "invalid aria2 in-flight padding"
                );
            }
            partial[index] = (0..chunks)
                .map(|i| field[i / 8] & (0x80 >> (i % 8)) != 0)
                .collect();
        }
        if c.position() != bytes.len() as u64 {
            bail!("trailing aria2 control data");
        }
        Ok(Self {
            total,
            piece_length,
            complete,
            partial,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&self.piece_length.to_be_bytes());
        out.extend_from_slice(&self.total.to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        let mut field = vec![0; self.complete.len().div_ceil(8)];
        for (i, done) in self.complete.iter().enumerate() {
            if *done {
                field[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out.extend_from_slice(&(field.len() as u32).to_be_bytes());
        out.extend_from_slice(&field);
        let in_flight = self.partial.iter().filter(|p| p.iter().any(|b| *b)).count();
        out.extend_from_slice(&(in_flight as u32).to_be_bytes());
        for (i, chunks) in self.partial.iter().enumerate() {
            if !chunks.iter().any(|b| *b) {
                continue;
            }
            let length = (self.total - i as u64 * u64::from(self.piece_length))
                .min(u64::from(self.piece_length));
            out.extend_from_slice(&(i as u32).to_be_bytes());
            out.extend_from_slice(&(length as u32).to_be_bytes());
            let mut bits = vec![0; chunks.len().div_ceil(8)];
            for (j, done) in chunks.iter().enumerate() {
                if *done {
                    bits[j / 8] |= 0x80 >> (j % 8);
                }
            }
            out.extend_from_slice(&(bits.len() as u32).to_be_bytes());
            out.extend_from_slice(&bits);
        }
        out
    }
}
