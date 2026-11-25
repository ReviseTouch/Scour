//! `$Secure:$SDS`, every security descriptor of the volume once: 256 KiB
//! blocks, each followed by its mirror, entries 16-byte aligned, none
//! crossing a block. An entry is its hash, its id, its own offset, its
//! length with this header, then the descriptor, self-relative. A freed
//! entry leaves zeros behind, and the entries go on after it (PC2's data
//! disk); an entry is known by its offset naming its own place and its hash
//! matching its descriptor.

use std::collections::{HashMap, HashSet};

const BLOCK: usize = 256 << 10;
const HEAD: usize = 20;

/// The descriptors of these ids, as found in the stream.
pub(crate) fn wanted(sds: &[u8], ids: &HashSet<u32>) -> HashMap<u32, Vec<u8>> {
    let mut out = HashMap::new();
    let mut at = 0usize;
    while at + HEAD <= sds.len() && out.len() < ids.len() {
        // The odd blocks are mirrors.
        if (at / BLOCK) % 2 == 1 {
            at = (at / BLOCK + 1) * BLOCK;
            continue;
        }
        let hash = u32::from_le_bytes(sds[at..at + 4].try_into().unwrap_or_default());
        let id = u32::from_le_bytes(sds[at + 4..at + 8].try_into().unwrap_or_default());
        let off = u64::from_le_bytes(sds[at + 8..at + 16].try_into().unwrap_or_default());
        let len = u32::from_le_bytes(sds[at + 16..at + 20].try_into().unwrap_or_default()) as usize;
        let fits = at % BLOCK + len <= BLOCK && at + len <= sds.len();
        if len < HEAD || off != at as u64 || !fits || hash_of(&sds[at + HEAD..at + len]) != hash {
            at += 16;
            continue;
        }
        if ids.contains(&id) {
            out.entry(id)
                .or_insert_with(|| sds[at + HEAD..at + len].to_vec());
        }
        at += len.next_multiple_of(16);
    }
    out
}

/// NTFS's hash of a descriptor: each 32-bit word added to the hash turned
/// left by three.
fn hash_of(sd: &[u8]) -> u32 {
    sd.as_chunks::<4>().0.iter().fold(0u32, |h, w| {
        h.rotate_left(3).wrapping_add(u32::from_le_bytes(*w))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sds: &mut Vec<u8>, id: u32, sd: &[u8]) {
        let at = sds.len();
        let len = HEAD + sd.len();
        sds.extend_from_slice(&hash_of(sd).to_le_bytes());
        sds.extend_from_slice(&id.to_le_bytes());
        sds.extend_from_slice(&(at as u64).to_le_bytes());
        sds.extend_from_slice(&(len as u32).to_le_bytes());
        sds.extend_from_slice(sd);
        sds.resize(at + len.next_multiple_of(16), 0);
    }

    #[test]
    fn descriptors_are_found_in_blocks_and_mirrors_skipped() {
        let mut sds = Vec::new();
        entry(&mut sds, 0x100, b"first...");
        // A freed entry: zeros, and the next one after them.
        sds.resize(sds.len() + 96, 0);
        entry(&mut sds, 0x101, b"second one..");
        // An entry whose hash does not match is not one.
        let at = sds.len();
        entry(&mut sds, 0x103, b"forged..");
        sds[at] ^= 1;
        sds.resize(BLOCK, 0);
        // The mirror holds the same entries at their first offsets: skipped.
        let mirror = sds.clone();
        sds.extend_from_slice(&mirror);
        entry(&mut sds, 0x102, b"third...");
        let ids: HashSet<u32> = [0x101, 0x102, 0x103, 0x999].into();
        let got = wanted(&sds, &ids);
        assert_eq!(got.len(), 2);
        assert_eq!(got[&0x101], b"second one..");
        assert_eq!(got[&0x102], b"third...");
    }
}
