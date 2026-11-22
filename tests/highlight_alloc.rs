//! Highlighting looks at every line before the parser gets it, for anything to shorten (a long
//! run, chain or YAML stretch); an ordinary line — nothing in it that long, short or not — is
//! looked at without allocating, in code and in YAML.
//!
//! A test binary of its own: its counting allocator sees every allocation of the binary it is
//! in, and in the library's test binary it slowed all of those tests by about a third, past the
//! time budgets some of them keep.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The system allocator, counting the allocations of each thread.
struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The allocations `f` makes on this thread.
fn allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    f();
    ALLOCATIONS.with(Cell::get) - before
}

/// The longest run, chain or stretch handed to the parser as it is (the library's `RUN_MAX`).
const RUN_MAX: usize = 128;

#[test]
fn an_ordinary_line_is_looked_at_without_allocating() {
    let long_code =
        "let total = first.value + second.value * third(count, limit) - fourth.offset; \
                     // a comment that runs on, past the length of any shortened run"
            .to_string();
    let pairs: Vec<String> = (0..30).map(|i| format!("k{i}: v{i}")).collect();
    let long_yaml = format!("key: {{{}}}  # and a comment", pairs.join(", "));
    assert!(long_code.chars().count() > RUN_MAX && long_yaml.chars().count() > RUN_MAX);
    for (line, yaml) in [
        ("let x = 1;", false),
        (long_code.as_str(), false),
        ("key: value", true),
        (long_yaml.as_str(), true),
    ] {
        let mut shortened = true;
        let n = allocations(|| shortened = sopen::gui::text::shortens(line, yaml));
        assert!(!shortened, "{line:.30}…: shortened");
        assert_eq!(n, 0, "{line:.30}… (yaml: {yaml}): {n} allocations");
    }
    // What is shortened is still found, and built.
    let chain = format!("v = {}x;", "x.".repeat(100));
    let mut shortened = false;
    let n = allocations(|| shortened = sopen::gui::text::shortens(&chain, false));
    assert!(
        shortened && n > 0,
        "a long chain: shortened {shortened}, {n} allocations"
    );
}
