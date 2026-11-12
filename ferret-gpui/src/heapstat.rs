//! What the process's heaps hold, for memory tests (the script runner's `heaps` step): each
//! heap's blocks in use and free, by size, and the biggest ones with how they begin (a PNG, a
//! picture's pixels, text), to tell memory that's still held from memory the heap keeps after
//! it was freed. Walks every heap with it locked: a test's tool, never run otherwise.

use std::fmt::Write;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Memory::{GetProcessHeap, GetProcessHeaps, HeapLock, HeapUnlock, HeapWalk, PROCESS_HEAP_ENTRY};

const BUSY: u16 = 4;
const REGION: u16 = 1;
const UNCOMMITTED: u16 = 2;
const MB: f64 = 1048576.;

/// A report of every heap, one line each, then the process heap's blocks in use by size and
/// its biggest ones.
pub fn report() -> String {
    let mut heaps = [HANDLE::default(); 64];
    let n = unsafe { GetProcessHeaps(&mut heaps) } as usize;
    let own = unsafe { GetProcessHeap() }.unwrap_or_default();
    let mut out = String::new();
    for &heap in &heaps[..n.min(64)] {
        if unsafe { HeapLock(heap) }.is_err() {
            continue;
        }
        let (mut busy, mut busy_n, mut free, mut uncommitted, mut regions) = (0u64, 0u64, 0u64, 0u64, 0u64);
        // In use by size: up to 1 KB, 16 KB, 256 KB, 1 MB, 4 MB, 16 MB, more.
        let limits = [1u64 << 10, 16 << 10, 256 << 10, 1 << 20, 4 << 20, 16 << 20, u64::MAX];
        let mut by_size = [(0u64, 0u64); 7];
        let mut biggest: Vec<(u64, [u8; 12])> = Vec::new();
        let mut e = PROCESS_HEAP_ENTRY::default();
        while unsafe { HeapWalk(heap, &mut e) }.is_ok() {
            let size = e.cbData as u64;
            if e.wFlags & BUSY != 0 {
                busy += size;
                busy_n += 1;
                let i = limits.iter().position(|l| size <= *l).unwrap_or(6);
                by_size[i].0 += 1;
                by_size[i].1 += size;
                if size >= 1 << 20 && heap == own {
                    let mut head = [0u8; 12];
                    // SAFETY: a block in use of at least 1 MB, with the heap locked.
                    unsafe { std::ptr::copy_nonoverlapping(e.lpData as *const u8, head.as_mut_ptr(), 12) };
                    biggest.push((size, head));
                }
            } else if e.wFlags & REGION != 0 {
                regions += 1;
            } else if e.wFlags & UNCOMMITTED != 0 {
                uncommitted += size;
            } else {
                free += size;
            }
        }
        let _ = unsafe { HeapUnlock(heap) };
        if busy + free < 1 << 20 {
            continue;
        }
        let _ = write!(
            out,
            "heap{}: in use {:.1} MB in {} blocks, free {:.1} MB, uncommitted {:.1} MB, {} regions; in use by size",
            if heap == own { " (the app's)" } else { "" },
            busy as f64 / MB,
            busy_n,
            free as f64 / MB,
            uncommitted as f64 / MB,
            regions
        );
        for (label, (count, bytes)) in ["<=1K", "<=16K", "<=256K", "<=1M", "<=4M", "<=16M", "more"].iter().zip(by_size) {
            let _ = write!(out, " {label}: {count} ({:.1} MB)", bytes as f64 / MB);
        }
        biggest.sort_by(|a, b| b.0.cmp(&a.0));
        // The big ones, grouped by size and how they begin.
        let mut groups: Vec<(u64, [u8; 12], u32)> = Vec::new();
        for (size, head) in biggest {
            match groups.iter_mut().find(|g| g.0 == size && g.1[..4] == head[..4]) {
                Some(g) => g.2 += 1,
                None => groups.push((size, head, 1)),
            }
        }
        for (size, head, count) in groups.into_iter().take(14) {
            let _ = write!(out, " | {count} x {:.1} MB starting {}", size as f64 / MB, head.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
        out.push('\n');
    }
    out
}
