//! Write protection for USB mass storage ("read-only mode"), enforced on the exporter.
//!
//! A hardware write-blocker does two things and so does this: it *tells* the host the disk is write-protected (the
//! write-protect bit in MODE SENSE, so every OS mounts it read-only on its own), and it *refuses* writes anyway
//! (anything that is not a known read-only SCSI command is answered locally with CHECK CONDITION and never reaches the
//! stick). It works for USB Bulk-Only Transport storage, which is almost every stick and drive; other device kinds
//! cannot be write-protected this way and are refused in read-only mode instead of silently left writable.
//!
//! The guard is a small state machine fed with the importer's bulk URBs (`on_submit`) and the data the device returns
//! (`on_complete`). It never touches the device itself.

/// What to do with a URB.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Send it to the real device.
    Forward,
    /// Answer it here: `status` is bytes accepted (OUT) or 0 / a negative errno (IN), `data` is the IN payload.
    Complete { status: i32, data: Vec<u8> },
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Why {
    WriteProtected,
    InvalidCommand,
    /// A REQUEST SENSE answered with the sense data of the command we blocked.
    Sense(SenseKind),
}
#[derive(Debug, PartialEq, Clone, Copy)]
enum SenseKind {
    DataProtect,
    IllegalRequest,
}

#[derive(Debug)]
struct Blocked {
    tag: u32,
    data_len: u32,
    dir_in: bool,
    got: u32,
    data_done: bool,
    why: Why,
}

/// A stretch of the drive whose bytes are replaced in what the importer reads (the drive itself is never touched).
pub struct Patch {
    /// First sector.
    pub lba: u64,
    /// The bytes to show instead (a whole number of 512-byte sectors).
    pub bytes: Vec<u8>,
}

/// A read command whose data stage overlaps a patch: where the data stage has got to.
struct ReadCmd {
    tag: u32,
    lba: u64,
    total: u64,
    done: u64,
}

pub struct Guard {
    ep_out: u8,
    ep_in: u8,
    blocked: Option<Blocked>,
    /// Sense data the next REQUEST SENSE should return (set when we block a command).
    next_sense: Option<SenseKind>,
    /// MODE SENSE opcode whose data stage we still have to patch.
    mode_sense: Option<u8>,
    patches: Vec<Patch>,
    read: Option<ReadCmd>,
}

/// First sector and sector count of a READ(6/10/12/16), if `cdb` is one.
fn read_range(cdb: &[u8]) -> Option<(u64, u64)> {
    let be = |a: usize, n: usize| cdb.get(a..a + n).map(|b| b.iter().fold(0u64, |v, &x| (v << 8) | x as u64));
    match *cdb.first()? {
        0x08 => Some((((cdb.get(1)? & 0x1f) as u64) << 16 | be(2, 2)?, if *cdb.get(4)? == 0 { 256 } else { *cdb.get(4)? as u64 })),
        0x28 => Some((be(2, 4)?, be(7, 2)?)),
        0xa8 => Some((be(2, 4)?, be(6, 4)?)),
        0x88 => Some((be(2, 8)?, be(10, 4)?)),
        _ => None,
    }
}

fn is_cbw(b: &[u8]) -> bool {
    b.len() == 31 && b.starts_with(b"USBC")
}

/// Known read-only SCSI commands. Anything else is blocked while read-only is on.
fn allowed(cdb: &[u8]) -> bool {
    match cdb[0] {
        0x00 | 0x12 | 0x1a | 0x5a | 0x25 | 0x08 | 0x28 | 0xa8 | 0x88 | 0x23 | 0x1e | 0x1b | 0x35 | 0x91 | 0xa0 | 0x43 | 0x46 | 0x4a | 0xa2 | 0x03 => true,
        0x9e => cdb.get(1).map(|b| b & 0x1f == 0x10).unwrap_or(false), // SERVICE ACTION IN(16): READ CAPACITY(16) only
        _ => false,
    }
}

/// Commands that change the medium or the device: reported as DATA PROTECT; the rest as ILLEGAL REQUEST.
fn is_write(cdb: &[u8]) -> bool {
    matches!(cdb[0], 0x0a | 0x2a | 0xaa | 0x8a | 0x2e | 0xae | 0x8e | 0x41 | 0x93 | 0x42 | 0x04 | 0x3b | 0xb5 | 0x15 | 0x55 | 0x85 | 0xa1)
}

fn sense_data(kind: SenseKind) -> [u8; 18] {
    let (key, asc) = match kind {
        SenseKind::DataProtect => (0x07, 0x27),
        SenseKind::IllegalRequest => (0x05, 0x20),
    };
    let mut s = [0u8; 18];
    s[0] = 0x70; // current error, fixed format
    s[2] = key;
    s[7] = 0x0a; // additional length
    s[12] = asc;
    s
}

fn csw(tag: u32, residue: u32, status: u8) -> Vec<u8> {
    let mut c = Vec::with_capacity(13);
    c.extend_from_slice(b"USBS");
    c.extend_from_slice(&tag.to_le_bytes());
    c.extend_from_slice(&residue.to_le_bytes());
    c.push(status);
    c
}

impl Guard {
    pub fn new(ep_out: u8, ep_in: u8) -> Self {
        Guard { ep_out, ep_in, blocked: None, next_sense: None, mode_sense: None, patches: vec![], read: None }
    }

    /// Replace these stretches of the drive in what reads return (see `ntfs`). Only reads are touched.
    pub fn set_patches(&mut self, patches: Vec<Patch>) {
        self.patches = patches;
    }

    /// `ep` is the endpoint number without the direction bit; `out` is the OUT payload (empty for IN).
    pub fn on_submit(&mut self, ep: u8, is_in: bool, len: usize, out: &[u8]) -> Verdict {
        let (ep_out, ep_in) = (self.ep_out & 0x0f, self.ep_in & 0x0f);
        if ep == ep_out && !is_in {
            if is_cbw(out) {
                self.blocked = None; // a new command: whatever the host abandoned is gone
                self.read = None;
                return self.command(out);
            }
            if let Some(b) = self.blocked.as_mut() {
                if !b.dir_in {
                    // data stage of a command we are not passing on: swallow it
                    b.got = b.got.saturating_add(len as u32);
                    if b.got >= b.data_len {
                        b.data_done = true;
                    }
                    return Verdict::Complete { status: len as i32, data: vec![] };
                }
            }
            return Verdict::Forward;
        }
        if ep == ep_in && is_in {
            if let Some(b) = self.blocked.as_mut() {
                if b.dir_in && b.data_len > 0 && !b.data_done {
                    b.data_done = true;
                    return match b.why {
                        Why::Sense(kind) => {
                            let s = sense_data(kind);
                            let n = (b.data_len as usize).min(len).min(s.len());
                            b.got = n as u32;
                            Verdict::Complete { status: 0, data: s[..n].to_vec() }
                        }
                        // STALL the data stage; the host clears the halt and reads the status
                        _ => Verdict::Complete { status: -32, data: vec![] },
                    };
                }
                if len >= 13 {
                    let (tag, residue) = (b.tag, b.data_len.saturating_sub(if matches!(b.why, Why::Sense(_)) { b.got } else { 0 }));
                    let status = if matches!(b.why, Why::Sense(_)) { 0 } else { 1 };
                    self.blocked = None;
                    return Verdict::Complete { status: 0, data: csw(tag, residue, status) };
                }
            }
        }
        Verdict::Forward
    }

    fn command(&mut self, cbw: &[u8]) -> Verdict {
        let tag = u32::from_le_bytes([cbw[4], cbw[5], cbw[6], cbw[7]]);
        let data_len = u32::from_le_bytes([cbw[8], cbw[9], cbw[10], cbw[11]]);
        let dir_in = cbw[12] & 0x80 != 0;
        let cdb_len = (cbw[14] as usize).clamp(1, 16);
        let cdb = &cbw[15..15 + cdb_len];
        let block = |why, kind, this: &mut Self| {
            this.next_sense = Some(kind);
            this.blocked = Some(Blocked { tag, data_len, dir_in, got: 0, data_done: data_len == 0, why });
            Verdict::Complete { status: 31, data: vec![] }
        };
        if cdb[0] == 0x03 {
            // REQUEST SENSE: answer it ourselves if it is about a command we blocked, else let the device answer
            return match self.next_sense.take() {
                Some(kind) => {
                    self.blocked = Some(Blocked { tag, data_len, dir_in: true, got: 0, data_done: data_len == 0, why: Why::Sense(kind) });
                    Verdict::Complete { status: 31, data: vec![] }
                }
                None => Verdict::Forward,
            };
        }
        if allowed(cdb) {
            self.mode_sense = matches!(cdb[0], 0x1a | 0x5a).then_some(cdb[0]);
            if let Some((lba, blocks)) = read_range(cdb).filter(|_| !self.patches.is_empty()) {
                let total = (blocks.saturating_mul(512)).min(data_len as u64);
                let end = lba.saturating_add(blocks);
                if self.patches.iter().any(|p| p.lba < end && lba < p.lba + (p.bytes.len() / 512) as u64) {
                    self.read = Some(ReadCmd { tag, lba, total, done: 0 });
                }
            }
            return Verdict::Forward;
        }
        if is_write(cdb) {
            block(Why::WriteProtected, SenseKind::DataProtect, self)
        } else {
            block(Why::InvalidCommand, SenseKind::IllegalRequest, self)
        }
    }

    /// Called with the data a forwarded IN transfer returned: sets the write-protect bit in MODE SENSE data and applies
    /// the patches to reads.
    pub fn on_complete(&mut self, ep: u8, is_in: bool, data: &mut [u8]) {
        if !(is_in && ep == self.ep_in & 0x0f) {
            return;
        }
        if let Some(rc) = self.read.as_mut() {
            if data.len() == 13 && data.starts_with(b"USBS") && data[4..8] == rc.tag.to_le_bytes() {
                self.read = None; // this command's status block after a short data stage: not data
                return;
            }
            let (from, to) = (rc.done as i128, rc.done as i128 + data.len() as i128);
            for p in &self.patches {
                let at = (p.lba as i128 - rc.lba as i128) * 512; // where the patch starts, relative to this command's data
                let (a, b) = (from.max(at), to.min(at + p.bytes.len() as i128));
                if a < b {
                    data[(a - from) as usize..(b - from) as usize].copy_from_slice(&p.bytes[(a - at) as usize..(b - at) as usize]);
                }
            }
            rc.done += data.len() as u64;
            if rc.done >= rc.total {
                self.read = None;
            }
            return;
        }
        if let Some(op) = self.mode_sense {
            if data.len() >= 4 && !data.starts_with(b"USBS") {
                // MODE SENSE(6) header byte 2, MODE SENSE(10) header byte 3: DEVICE-SPECIFIC PARAMETER, bit 7 = WP
                let i = if op == 0x1a { 2 } else { 3 };
                data[i] |= 0x80;
                self.mode_sense = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cbw(tag: u32, len: u32, dir_in: bool, cdb: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 31];
        b[0..4].copy_from_slice(b"USBC");
        b[4..8].copy_from_slice(&tag.to_le_bytes());
        b[8..12].copy_from_slice(&len.to_le_bytes());
        b[12] = if dir_in { 0x80 } else { 0 };
        b[14] = cdb.len() as u8;
        b[15..15 + cdb.len()].copy_from_slice(cdb);
        b
    }
    fn g() -> Guard {
        Guard::new(0x02, 0x81)
    }

    #[test]
    fn reads_and_probes_pass_through() {
        let mut g = g();
        for cdb in
            [&[0x00, 0, 0, 0, 0, 0][..], &[0x12, 0, 0, 0, 36, 0], &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[0x28, 0, 0, 0, 0, 0, 0, 0, 8, 0], &[0x9e, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0]]
        {
            assert_eq!(g.on_submit(2, false, 31, &cbw(1, 8, true, cdb)), Verdict::Forward, "{cdb:02x?}");
        }
        assert_eq!(g.on_submit(1, true, 512, &[]), Verdict::Forward);
    }

    #[test]
    fn a_write_is_swallowed_and_fails_with_data_protect() {
        let mut g = g();
        // WRITE(10), 1024 bytes
        assert_eq!(g.on_submit(2, false, 31, &cbw(0x1111, 1024, false, &[0x2a, 0, 0, 0, 0, 0, 0, 0, 2, 0])), Verdict::Complete { status: 31, data: vec![] });
        // its data stage is swallowed (accepted, never written), in two pieces
        assert_eq!(g.on_submit(2, false, 512, &[0; 512]), Verdict::Complete { status: 512, data: vec![] });
        assert_eq!(g.on_submit(2, false, 512, &[0; 512]), Verdict::Complete { status: 512, data: vec![] });
        // status: a failed CSW with our tag
        let Verdict::Complete { data, .. } = g.on_submit(1, true, 13, &[]) else { panic!() };
        assert_eq!(&data[..4], b"USBS");
        assert_eq!(u32::from_le_bytes([data[4], data[5], data[6], data[7]]), 0x1111);
        assert_eq!(data[12], 1, "status Failed");
        // the host then asks why: REQUEST SENSE gets DATA PROTECT
        assert_eq!(g.on_submit(2, false, 31, &cbw(0x2222, 18, true, &[0x03, 0, 0, 0, 18, 0])), Verdict::Complete { status: 31, data: vec![] });
        let Verdict::Complete { data, .. } = g.on_submit(1, true, 18, &[]) else { panic!() };
        assert_eq!((data[2], data[12]), (0x07, 0x27));
        let Verdict::Complete { data, .. } = g.on_submit(1, true, 13, &[]) else { panic!() };
        assert_eq!((data[12], u32::from_le_bytes([data[4], data[5], data[6], data[7]])), (0, 0x2222), "REQUEST SENSE itself succeeds");
        // and the guard is idle again
        assert_eq!(g.on_submit(2, false, 31, &cbw(3, 0, false, &[0; 6])), Verdict::Forward);
    }

    #[test]
    fn every_write_class_command_is_blocked() {
        for op in [0x0a, 0x2a, 0xaa, 0x8a, 0x2e, 0xae, 0x8e, 0x41, 0x93, 0x42, 0x04, 0x3b, 0xb5, 0x15, 0x55, 0x85, 0xa1] {
            let mut g = g();
            assert!(matches!(g.on_submit(2, false, 31, &cbw(1, 0, false, &[op, 0, 0, 0, 0, 0])), Verdict::Complete { status: 31, .. }), "{op:#x}");
        }
    }

    #[test]
    fn unknown_commands_are_refused_as_illegal_and_stall_their_data() {
        let mut g = g();
        // a vendor command expecting data IN
        assert_eq!(g.on_submit(2, false, 31, &cbw(9, 64, true, &[0xd0, 0, 0, 0, 0, 0])), Verdict::Complete { status: 31, data: vec![] });
        assert_eq!(g.on_submit(1, true, 64, &[]), Verdict::Complete { status: -32, data: vec![] });
        let Verdict::Complete { data, .. } = g.on_submit(1, true, 13, &[]) else { panic!() };
        assert_eq!(data[12], 1);
        assert_eq!(g.on_submit(2, false, 31, &cbw(10, 18, true, &[0x03, 0, 0, 0, 18, 0])), Verdict::Complete { status: 31, data: vec![] });
        let Verdict::Complete { data, .. } = g.on_submit(1, true, 18, &[]) else { panic!() };
        assert_eq!((data[2], data[12]), (0x05, 0x20));
    }

    #[test]
    fn request_sense_without_a_blocked_command_goes_to_the_device() {
        let mut g = g();
        assert_eq!(g.on_submit(2, false, 31, &cbw(1, 18, true, &[0x03, 0, 0, 0, 18, 0])), Verdict::Forward);
    }

    #[test]
    fn mode_sense_reports_write_protected() {
        let mut g = g();
        assert_eq!(g.on_submit(2, false, 31, &cbw(1, 192, true, &[0x1a, 0, 0x3f, 0, 192, 0])), Verdict::Forward);
        let mut d = [0x03, 0x00, 0x00, 0x00];
        g.on_complete(1, true, &mut d);
        assert_eq!(d[2] & 0x80, 0x80);
        // MODE SENSE(10): byte 3
        assert_eq!(g.on_submit(2, false, 31, &cbw(2, 192, true, &[0x5a, 0, 0x3f, 0, 0, 0, 0, 0, 192, 0])), Verdict::Forward);
        let mut d = [0x00, 0x06, 0x00, 0x00, 0, 0, 0, 0];
        g.on_complete(1, true, &mut d);
        assert_eq!(d[3] & 0x80, 0x80);
        // a status block is never patched
        assert_eq!(g.on_submit(2, false, 31, &cbw(3, 192, true, &[0x1a, 0, 0x3f, 0, 192, 0])), Verdict::Forward);
        let mut csw = *b"USBS\x03\0\0\0\0\0\0\0\0";
        g.on_complete(1, true, &mut csw);
        assert_eq!(&csw, b"USBS\x03\0\0\0\0\0\0\0\0");
    }

    #[test]
    fn a_new_command_clears_an_abandoned_one() {
        let mut g = g();
        let _ = g.on_submit(2, false, 31, &cbw(1, 4096, false, &[0x2a, 0, 0, 0, 0, 0, 0, 0, 8, 0]));
        // the host gave up and sends a read: forwarded, state cleared
        assert_eq!(g.on_submit(2, false, 31, &cbw(2, 512, true, &[0x28, 0, 0, 0, 0, 0, 0, 0, 1, 0])), Verdict::Forward);
        assert_eq!(g.on_submit(1, true, 512, &[]), Verdict::Forward);
    }

    fn patched() -> Guard {
        let mut g = g();
        // sectors 100..104 (2 KiB) shown as 0xAA
        g.set_patches(vec![Patch { lba: 100, bytes: vec![0xaa; 2048] }]);
        g
    }
    fn read10(lba: u32, blocks: u16) -> Vec<u8> {
        let mut c = vec![0x28, 0];
        c.extend_from_slice(&lba.to_be_bytes());
        c.push(0);
        c.extend_from_slice(&blocks.to_be_bytes());
        c.push(0);
        c
    }

    #[test]
    fn a_read_that_covers_a_patch_gets_it_and_nothing_else_changes() {
        let mut g = patched();
        assert_eq!(g.on_submit(2, false, 31, &cbw(1, 4096 * 2, true, &read10(96, 16))), Verdict::Forward);
        let mut d = vec![0x11u8; 8192];
        g.on_complete(1, true, &mut d);
        assert!(d[..4 * 512].iter().all(|&b| b == 0x11), "sectors 96..100 untouched");
        assert!(d[4 * 512..8 * 512].iter().all(|&b| b == 0xaa), "sectors 100..104 patched");
        assert!(d[8 * 512..].iter().all(|&b| b == 0x11), "sectors 104.. untouched");
    }

    #[test]
    fn a_patch_is_applied_across_the_pieces_of_one_data_stage() {
        let mut g = patched();
        assert_eq!(g.on_submit(2, false, 31, &cbw(1, 8 * 512, true, &read10(98, 8))), Verdict::Forward);
        // the data arrives in three transfers that split the patch at odd places
        let mut all = vec![];
        for n in [700usize, 1000, 4096 - 1700] {
            let mut d = vec![0x11u8; n];
            g.on_complete(1, true, &mut d);
            all.extend(d);
        }
        assert_eq!(all.len(), 4096);
        // read starts at sector 98: patch covers bytes 1024..3072
        assert!(all[..1024].iter().all(|&b| b == 0x11) && all[1024..3072].iter().all(|&b| b == 0xaa) && all[3072..].iter().all(|&b| b == 0x11));
    }

    #[test]
    fn a_read_that_starts_inside_or_ends_inside_a_patch_gets_the_right_part() {
        let mut g = patched();
        // starts at sector 102 (inside), 4 sectors: 102,103 patched, 104,105 not
        assert_eq!(g.on_submit(2, false, 31, &cbw(1, 2048, true, &read10(102, 4))), Verdict::Forward);
        let mut d = vec![0x11u8; 2048];
        g.on_complete(1, true, &mut d);
        assert!(d[..1024].iter().all(|&b| b == 0xaa) && d[1024..].iter().all(|&b| b == 0x11));
        // ends at sector 101 (inside): sectors 99..101 -> 99 untouched, 100 patched
        assert_eq!(g.on_submit(2, false, 31, &cbw(2, 1024, true, &read10(99, 2))), Verdict::Forward);
        let mut d = vec![0x11u8; 1024];
        g.on_complete(1, true, &mut d);
        assert!(d[..512].iter().all(|&b| b == 0x11) && d[512..].iter().all(|&b| b == 0xaa));
    }

    #[test]
    fn reads_elsewhere_other_commands_and_the_status_block_are_never_touched() {
        let mut g = patched();
        for (lba, n) in [(0u32, 8u16), (96, 4), (104, 8), (4000, 1)] {
            assert_eq!(g.on_submit(2, false, 31, &cbw(1, n as u32 * 512, true, &read10(lba, n))), Verdict::Forward);
            let mut d = vec![0x11u8; n as usize * 512];
            g.on_complete(1, true, &mut d);
            assert!(d.iter().all(|&b| b == 0x11), "read at {lba} must not change");
        }
        // a command that is not a read
        assert_eq!(g.on_submit(2, false, 31, &cbw(2, 36, true, &[0x12, 0, 0, 0, 36, 0])), Verdict::Forward);
        let mut d = vec![0x11u8; 36];
        g.on_complete(1, true, &mut d);
        assert!(d.iter().all(|&b| b == 0x11));
        // a read of the patch that comes back short, followed by its status block: the block is not data
        assert_eq!(g.on_submit(2, false, 31, &cbw(0x77, 2048, true, &read10(100, 4))), Verdict::Forward);
        let mut csw = *b"USBS\x77\0\0\0\0\0\0\0\x01";
        g.on_complete(1, true, &mut csw);
        assert_eq!(&csw, b"USBS\x77\0\0\0\0\0\0\0\x01");
        // and a new command forgets an abandoned one
        assert_eq!(g.on_submit(2, false, 31, &cbw(3, 1024, true, &read10(100, 2))), Verdict::Forward);
        assert_eq!(g.on_submit(2, false, 31, &cbw(4, 512, true, &read10(0, 1))), Verdict::Forward);
        let mut d = vec![0x11u8; 512];
        g.on_complete(1, true, &mut d);
        assert!(d.iter().all(|&b| b == 0x11));
    }

    #[test]
    fn every_read_command_form_is_understood_and_no_patches_means_no_change() {
        let mut g = patched();
        let r6 = [0x08, 0, 0, 100, 4, 0]; // READ(6): a 21-bit block address in bytes 1..3 (here 100), 4 blocks
        let r12 = [0xa8, 0, 0, 0, 0, 100, 0, 0, 0, 4, 0, 0];
        let mut r16 = vec![0x88, 0, 0, 0, 0, 0, 0, 0, 0, 100, 0, 0, 0, 4, 0, 0];
        for cdb in [&r6[..], &r12[..], &r16[..]] {
            assert_eq!(g.on_submit(2, false, 31, &cbw(1, 2048, true, cdb)), Verdict::Forward);
            let mut d = vec![0x11u8; 2048];
            g.on_complete(1, true, &mut d);
            assert!(d.iter().all(|&b| b == 0xaa), "{:#x}", cdb[0]);
        }
        r16[9] = 200;
        assert_eq!(g.on_submit(2, false, 31, &cbw(1, 2048, true, &r16)), Verdict::Forward);
        let mut d = vec![0x11u8; 2048];
        g.on_complete(1, true, &mut d);
        assert!(d.iter().all(|&b| b == 0x11));
        // READ(6)'s address uses only the low 5 bits of byte 1; a count of 0 means 256
        assert_eq!(read_range(&[0x08, 0xff, 0x12, 0x34, 0, 0]), Some((0x1f1234, 256)));
        // a READ(16) past 2^32 sectors is still understood
        assert_eq!(read_range(&[0x88, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0]), Some((1u64 << 32, 8)));
        // without patches nothing is ever recorded
        let mut g = g_plain();
        assert_eq!(g.on_submit(2, false, 31, &cbw(1, 2048, true, &read10(100, 4))), Verdict::Forward);
        let mut d = vec![0x11u8; 2048];
        g.on_complete(1, true, &mut d);
        assert!(d.iter().all(|&b| b == 0x11));
    }
    fn g_plain() -> Guard {
        g()
    }

    #[test]
    fn patched_reads_are_still_read_only_and_writes_are_still_blocked() {
        let mut g = patched();
        // a write to the patched area is blocked exactly as before: nothing reaches the drive
        let v = g.on_submit(2, false, 31, &cbw(9, 2048, false, &[0x2a, 0, 0, 0, 0, 100, 0, 0, 4, 0]));
        assert_eq!(v, Verdict::Complete { status: 31, data: vec![] });
    }

    #[test]
    fn other_endpoints_are_untouched() {
        let mut g = g();
        assert_eq!(g.on_submit(0, false, 8, &[0; 8]), Verdict::Forward);
        assert_eq!(g.on_submit(3, true, 64, &[]), Verdict::Forward);
    }
}
