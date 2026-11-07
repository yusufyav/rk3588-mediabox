// An ELF note area's GNU build-id, as hex. Shared by build.rs (the
// libraries built against) and gst_abi.rs (the libraries loaded).

/// The NT_GNU_BUILD_ID note's descriptor in `notes` (one PT_NOTE's bytes,
/// little-endian), as lowercase hex.
pub fn build_id(notes: &[u8]) -> Option<String> {
    let word = |at: usize| -> Option<usize> { Some(u32::from_le_bytes(notes.get(at..at + 4)?.try_into().ok()?) as usize) };
    let align = |n: usize| n.div_ceil(4) * 4;
    let mut at = 0;
    while at + 12 <= notes.len() {
        let (name_size, desc_size, kind) = (word(at)?, word(at + 4)?, word(at + 8)?);
        let name = notes.get(at + 12..at + 12 + name_size)?;
        let desc_at = at + 12 + align(name_size);
        let desc = notes.get(desc_at..desc_at + desc_size)?;
        if kind == 3 && name == b"GNU\0" {
            return Some(desc.iter().map(|b| format!("{b:02x}")).collect());
        }
        at = desc_at + align(desc_size);
    }
    None
}
