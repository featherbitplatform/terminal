//! Shared setup for the compression-on variants of the tests that read history (export, search,
//! selection text, draw lists). A variant feeds the same output to a terminal with history
//! compression off and to one with it on, followed by enough filler lines that every history
//! row the test wrote lies in a compressed block, runs the history jobs, and then expects
//! byte-identical reads from both.

use crate::{GridSize, Terminal};

/// Rows kept uncompressed in the variants: the smallest setting, so every block but the newest
/// is compressed.
pub(crate) const KEEP: Option<usize> = Some(1);

/// Scrollback of the variants: far more than they write, so nothing is evicted.
const SCROLLBACK: usize = 200_000;

pub(crate) fn size(cols: u16, rows: u16) -> GridSize {
    GridSize {
        cols,
        rows,
        cell_width: 8.0,
        cell_height: 16.0,
        scale: 1.0,
    }
}

/// `n` filler lines numbered from `from`; no test searches for their text.
fn filler(from: usize, n: usize) -> Vec<u8> {
    (from..from + n)
        .flat_map(|i| format!("~~ {i:07}\r\n").into_bytes())
        .collect()
}

/// History blocks waiting to be compressed (taking jobs changes nothing).
fn pending(t: &mut Terminal) -> usize {
    t.next_history_jobs(usize::MAX).len()
}

/// Run every pending history job of `t` as the worker does (take them, run each off the
/// terminal, install the result), asserting that every result was installed and that no job is
/// left. Returns how many ran.
pub(crate) fn run_jobs(t: &mut Terminal) -> usize {
    let jobs = t.next_history_jobs(usize::MAX);
    let n = jobs.len();
    for job in jobs {
        let done = feathercritty::term::HistoryJob::run(job);
        assert!(t.install_history_job(done), "a result was refused");
    }
    // The flag stays up after an install until a look for more jobs finds none.
    assert!(t.next_history_jobs(usize::MAX).is_empty(), "every job ran");
    assert!(!t.has_history_jobs(), "no job is left");
    n
}

/// Two terminals of `cols` x `rows` fed `before`, filler, then `after`: the first with history
/// compression off, the second with it on ([`KEEP`]) and every history job run. The filler goes
/// on until two more blocks than `before` filled can be compressed, so every row of `before`,
/// including the screen rows that scroll into history after it, is in a compressed block.
pub(crate) fn pair(cols: u16, rows: u16, before: &[u8], after: &[u8]) -> (Terminal, Terminal) {
    let mut on = Terminal::new(size(cols, rows), SCROLLBACK);
    on.set_history_compression(KEEP);
    on.feed(before);
    let start = pending(&mut on);
    let (mut fed, mut lines) = (Vec::new(), 0);
    while pending(&mut on) < start + 2 {
        let more = filler(lines, 500);
        lines += 500;
        on.feed(&more);
        fed.extend(more);
    }
    on.feed(after);
    assert!(on.history_len() < SCROLLBACK, "nothing was evicted");
    assert!(run_jobs(&mut on) > 0, "blocks to compress");

    let mut off = Terminal::new(size(cols, rows), SCROLLBACK);
    off.feed(before);
    off.feed(&fed);
    off.feed(after);
    assert_eq!(off.history_len(), on.history_len());
    (off, on)
}
