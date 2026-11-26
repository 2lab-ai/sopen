//! EH-3 (`.prd/12-editor.md` §8, §14; E19): the heap the editor's kept highlighting states hold
//! over a 16 MiB Rust file at their spacing, 64 lines, is at most 32 MiB.
//!
//! A test binary of its own: its counting allocator sees every allocation of the binary it is
//! in, and in the library's test binary such an allocator slowed every test by about a third
//! (`tests/highlight_alloc.rs`).

use sopen::gui::edit::{KeptStates, STATE_LINES};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The system allocator, counting the bytes each thread has live: what it allocated less what
/// it freed. The states are made and kept on the test's thread.
struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn count(bytes: isize) {
    let _ = LIVE.try_with(|n| n.set(n.get() + bytes));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            count(layout.size() as isize);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        count(-(layout.size() as isize));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            count(new_size as isize - layout.size() as isize);
        }
        p
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn live() -> isize {
    LIVE.with(Cell::get)
}

/// Real Rust, ~50 bytes a line: this repository's text viewer.
const FILE: &str = include_str!("../src/gui/text.rs");

/// Copies of [`FILE`] to `size` bytes, cut after the last line that ends before.
fn rust_of(size: usize) -> Vec<u8> {
    let mut text = String::with_capacity(size + FILE.len());
    while text.len() < size {
        text.push_str(FILE);
    }
    let end = text[..size].rfind('\n').map_or(size, |i| i + 1);
    text.truncate(end);
    text.into_bytes()
}

/// The heap `fill` adds and keeps: the states, and the table of their lines' starts.
fn kept_heap(bytes: Vec<u8>) -> (usize, u64, isize) {
    sopen::gui::text::warm();
    // The parser's patterns compile at their first use and stay with the syntaxes: over one
    // copy of the file, before the measure.
    let mut warm = KeptStates::new(FILE.as_bytes().to_vec(), "warm.rs");
    warm.fill();
    drop(warm);
    let mut kept = KeptStates::new(bytes, "big.rs");
    let before = live();
    kept.fill();
    let heap = live() - before;
    (kept.count(), kept.lines(), heap)
}

/// Over `mib` MiB of Rust: the states one per 64 lines, their heap within EH-3's rate (32 MiB
/// for 16), printed.
fn within_eh3(mib: usize) {
    assert_eq!(STATE_LINES, 64);
    let (states, lines, heap) = kept_heap(rust_of(mib << 20));
    println!(
        "EH-3 over {mib} MiB: {states} kept states (every {STATE_LINES} lines of {lines}) hold \
         {heap} bytes ({:.2} MiB, {} bytes a state)",
        heap as f64 / (1 << 20) as f64,
        heap / states.max(1) as isize
    );
    assert_eq!(states as u64, (lines - 1) / STATE_LINES + 1);
    let bound = (32 << 20) * mib as isize / 16;
    assert!(heap <= bound, "{heap} bytes, more than {bound}");
}

/// EH-3 at an eighth of its size, in every run: 2 MiB, at most 4 MiB.
#[test]
fn eh3_the_kept_states_of_2_mib_of_rust_hold_at_most_4_mib() {
    within_eh3(2);
}

/// EH-3 as specified: 16 MiB, at most 32 MiB. Three and a half minutes in a debug build, so by
/// hand, as the buffer's measurements are: `cargo test --release --test kept_states_heap --
/// --ignored --nocapture` (2026-11-27, a debug build in 205 s: 6,168 states over 394,744 lines
/// held 19,643,861 bytes, 18.73 MiB).
#[test]
#[ignore]
fn eh3_the_kept_states_of_16_mib_of_rust_hold_at_most_32_mib() {
    within_eh3(16);
}
