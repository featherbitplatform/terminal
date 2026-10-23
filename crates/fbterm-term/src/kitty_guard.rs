//! Bounds the kitty keyboard protocol's flag stack before output reaches the emulator.
//!
//! Upstream bug (alacritty_terminal 0.26, `Term::push_keyboard_mode` in `term/mod.rs`): once the
//! keyboard mode stack reaches `KEYBOARD_MODE_STACK_MAX_DEPTH` (4096) it evicts with
//! `self.title_stack.remove(0)` instead of removing from the keyboard stack. With an empty title
//! stack that is `Vec::remove(0)` on an empty vector, which panics, so a program (or remote host)
//! that sends `CSI > flags u` 4097 times takes down the parser. Remove this module once the
//! upstream fix is released and adopted.
//!
//! The guard follows the emulator's parser closely enough to track the push depth of each screen
//! (pushes `CSI > flags u`, pops `CSI < n u`, the alternate screen swap `CSI ? 1049 h/l`, reset
//! `ESC c`) and drops pushes that would take a stack past [`MAX_DEPTH`]. Where it cannot be sure
//! it errs towards counting a push, never towards counting a pop. Only bytes of a candidate push
//! sequence are held back between chunks; everything else passes through in order.

use std::borrow::Cow;

/// Pushes beyond this depth are dropped. Far below the emulator's 4096; programs push a handful.
pub(crate) const MAX_DEPTH: u16 = 128;
/// Longest push sequence kept; longer ones are garbage and dropped whole.
const MAX_HELD: usize = 64;
/// More parameters than this and the parser may ignore the sequence: never count it as a pop.
const MAX_POP_PARAMS: u16 = 8;

const ESC: u8 = 0x1b;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    /// Held: ESC (plus ignored bytes).
    Esc,
    /// Held: ESC [ (plus ignored bytes).
    CsiEntry,
    /// Held: ESC [ > ... (a possible push).
    Push { invalid: bool, overflow: bool },
    /// Passed through: ESC [ < ... (a possible pop).
    Pop {
        value: u16,
        in_first: bool,
        params: u16,
        invalid: bool,
    },
    /// Passed through: ESC [ ? ... (private modes, for the alternate screen).
    Private {
        value: u16,
        in_sub: bool,
        params: u16,
        swap: bool,
        invalid: bool,
    },
    /// Passed through: any other CSI sequence, until its final byte.
    OtherCsi,
}

#[derive(Debug, Default)]
pub(crate) struct KittyGuard {
    state: State,
    held: Vec<u8>,
    /// Push depth of the active screen and of the inactive one.
    depth: [u16; 2],
    alt: bool,
}

fn is_c0(b: u8) -> bool {
    matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f)
}

fn is_final(b: u8) -> bool {
    (0x40..=0x7e).contains(&b)
}

impl KittyGuard {
    /// Forget all tracking (the emulator dropped its stacks).
    pub(crate) fn clear_depths(&mut self) {
        self.depth = [0, 0];
    }

    /// Bytes held back from a sequence still in progress.
    pub(crate) fn take_held(&mut self) -> Vec<u8> {
        self.state = State::Ground;
        std::mem::take(&mut self.held)
    }

    /// Align with the emulator's actual screen; a mismatch means the tracking drifted, so both
    /// stacks take the larger depth (over-counting is safe).
    pub(crate) fn sync_screen(&mut self, alt: bool) {
        if alt != self.alt {
            let d = self.depth[0].max(self.depth[1]);
            self.depth = [d, d];
            self.alt = alt;
        }
    }

    #[cfg(test)]
    pub(crate) fn depth(&self) -> u16 {
        self.depth[0]
    }

    /// Filter one chunk of output. Returns the bytes to hand to the emulator.
    pub(crate) fn filter<'a>(&mut self, input: &'a [u8]) -> Cow<'a, [u8]> {
        if self.state == State::Ground && !input.contains(&ESC) {
            return Cow::Borrowed(input);
        }
        let mut out = Vec::with_capacity(input.len() + self.held.len());
        let mut i = 0;
        while i < input.len() {
            if self.state == State::Ground {
                match input[i..].iter().position(|&b| b == ESC) {
                    Some(n) => {
                        out.extend_from_slice(&input[i..i + n]);
                        i += n + 1;
                        self.held.push(ESC);
                        self.state = State::Esc;
                    }
                    None => {
                        out.extend_from_slice(&input[i..]);
                        break;
                    }
                }
                continue;
            }
            self.step(input[i], &mut out);
            i += 1;
        }
        Cow::Owned(out)
    }

    fn flush(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.held);
    }

    /// A new ESC starts over (the parser treats ESC this way from every state).
    fn restart(&mut self, out: &mut Vec<u8>) {
        self.flush(out);
        self.held.push(ESC);
        self.state = State::Esc;
    }

    fn abort(&mut self, b: u8, out: &mut Vec<u8>) {
        self.flush(out);
        out.push(b);
        self.state = State::Ground;
    }

    fn step(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Ground => unreachable!("ground bytes are copied in bulk"),
            State::Esc => match b {
                // A repeated ESC is ignored by the parser.
                ESC => {}
                0x18 | 0x1a => self.abort(b, out),
                b'[' => {
                    self.held.push(b);
                    self.state = State::CsiEntry;
                }
                b'c' => {
                    self.depth = [0, 0];
                    self.alt = false;
                    self.abort(b, out);
                }
                _ if is_c0(b) || b >= 0x7f => self.held.push(b),
                _ => self.abort(b, out),
            },
            State::CsiEntry => match b {
                b'>' => {
                    self.held.push(b);
                    self.state = State::Push {
                        invalid: false,
                        overflow: false,
                    };
                }
                b'<' => {
                    self.abort(b, out);
                    self.state = State::Pop {
                        value: 0,
                        in_first: true,
                        params: 0,
                        invalid: false,
                    };
                }
                b'?' => {
                    self.abort(b, out);
                    self.state = State::Private {
                        value: 0,
                        in_sub: false,
                        params: 0,
                        swap: false,
                        invalid: false,
                    };
                }
                ESC => self.restart(out),
                0x18 | 0x1a => self.abort(b, out),
                _ if is_c0(b) || b >= 0x7f => self.held.push(b),
                _ if is_final(b) => self.abort(b, out),
                _ => {
                    self.abort(b, out);
                    self.state = State::OtherCsi;
                }
            },
            State::Push {
                mut invalid,
                mut overflow,
            } => match b {
                ESC => self.restart(out),
                0x18 | 0x1a => self.abort(b, out),
                _ if is_final(b) => {
                    let push = b == b'u' && !invalid;
                    let active = &mut self.depth[0];
                    let keep = if push {
                        if *active >= MAX_DEPTH || overflow {
                            false
                        } else {
                            *active += 1;
                            true
                        }
                    } else {
                        !overflow
                    };
                    if keep {
                        self.abort(b, out);
                    } else {
                        // Controls inside the sequence still execute.
                        out.extend(self.held.iter().copied().filter(|&c| is_c0(c)));
                        self.held.clear();
                        self.state = State::Ground;
                    }
                }
                _ => {
                    match b {
                        0x20..=0x2f | 0x3c..=0x3f => invalid = true,
                        _ => {}
                    }
                    if self.held.len() < MAX_HELD {
                        self.held.push(b);
                    } else {
                        overflow = true;
                    }
                    self.state = State::Push { invalid, overflow };
                }
            },
            State::Pop {
                mut value,
                mut in_first,
                mut params,
                mut invalid,
            } => {
                out.push(b);
                match b {
                    ESC => {
                        out.pop();
                        self.restart(out);
                        return;
                    }
                    0x18 | 0x1a => {
                        self.state = State::Ground;
                        return;
                    }
                    b'0'..=b'9' if in_first => {
                        value = value.saturating_mul(10).saturating_add(u16::from(b - b'0'));
                    }
                    b':' | b';' => {
                        in_first = false;
                        params += 1;
                        invalid |= params > MAX_POP_PARAMS;
                    }
                    0x20..=0x2f | 0x3c..=0x3f => invalid = true,
                    _ if is_final(b) => {
                        if b == b'u' && !invalid {
                            let n = if value == 0 { 1 } else { value };
                            self.depth[0] = self.depth[0].saturating_sub(n);
                        }
                        self.state = State::Ground;
                        return;
                    }
                    _ => {}
                }
                self.state = State::Pop {
                    value,
                    in_first,
                    params,
                    invalid,
                };
            }
            State::Private {
                mut value,
                mut in_sub,
                mut params,
                mut swap,
                mut invalid,
            } => {
                out.push(b);
                match b {
                    ESC => {
                        out.pop();
                        self.restart(out);
                        return;
                    }
                    0x18 | 0x1a => {
                        self.state = State::Ground;
                        return;
                    }
                    b'0'..=b'9' if !in_sub => {
                        value = value.saturating_mul(10).saturating_add(u16::from(b - b'0'));
                    }
                    b':' => in_sub = true,
                    b';' => {
                        swap |= value == 1049;
                        value = 0;
                        in_sub = false;
                        params += 1;
                    }
                    0x20..=0x2f | 0x3c..=0x3f => invalid = true,
                    _ if is_final(b) => {
                        swap |= value == 1049;
                        // A sequence with many parameters may be ignored by the parser; the
                        // screen is re-synced from the emulator after each chunk anyway.
                        if swap && !invalid && params < 30 {
                            let to_alt = match b {
                                b'h' => Some(true),
                                b'l' => Some(false),
                                _ => None,
                            };
                            if let Some(alt) = to_alt
                                && alt != self.alt
                            {
                                self.depth.swap(0, 1);
                                self.alt = alt;
                            }
                        }
                        self.state = State::Ground;
                        return;
                    }
                    _ => {}
                }
                self.state = State::Private {
                    value,
                    in_sub,
                    params,
                    swap,
                    invalid,
                };
            }
            State::OtherCsi => match b {
                ESC => self.restart(out),
                0x18 | 0x1a => {
                    out.push(b);
                    self.state = State::Ground;
                }
                _ => {
                    out.push(b);
                    if is_final(b) {
                        self.state = State::Ground;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(g: &mut KittyGuard, chunks: &[&[u8]]) -> Vec<u8> {
        chunks
            .iter()
            .flat_map(|c| g.filter(c).into_owned())
            .collect()
    }

    #[test]
    fn plain_output_passes_borrowed() {
        let mut g = KittyGuard::default();
        assert!(matches!(g.filter(b"hello"), Cow::Borrowed(_)));
        assert_eq!(
            run(&mut g, &[b"a\x1b[31mb\x1b[>4;1m\x1b[>c"]),
            b"a\x1b[31mb\x1b[>4;1m\x1b[>c"
        );
    }

    #[test]
    fn pushes_past_the_cap_are_dropped() {
        let mut g = KittyGuard::default();
        let push = b"\x1b[>1u";
        let mut input = Vec::new();
        for _ in 0..200 {
            input.extend_from_slice(push);
        }
        let out = run(&mut g, &[&input]);
        assert_eq!(out.len(), push.len() * usize::from(MAX_DEPTH));
        assert_eq!(g.depth(), MAX_DEPTH);
        // A pop makes room for exactly one more.
        let out = run(&mut g, &[b"\x1b[<u\x1b[>1u\x1b[>1u"]);
        assert_eq!(out, b"\x1b[<u\x1b[>1u");
        let out = run(&mut g, &[b"\x1b[<5u"]);
        assert_eq!(out, b"\x1b[<5u");
        assert_eq!(g.depth(), MAX_DEPTH - 5);
    }

    #[test]
    fn split_sequences_are_handled_across_chunks() {
        let mut g = KittyGuard::default();
        let mut total = Vec::new();
        for _ in 0..300 {
            total.extend(run(&mut g, &[b"x\x1b", b"[", b">", b"3", b"u"]));
        }
        assert_eq!(g.depth(), MAX_DEPTH);
        let kept = total.windows(5).filter(|w| *w == b"\x1b[>3u").count();
        assert_eq!(kept, usize::from(MAX_DEPTH));
        assert_eq!(total.iter().filter(|&&b| b == b'x').count(), 300);
    }

    #[test]
    fn not_a_push_passes_and_does_not_count() {
        let mut g = KittyGuard::default();
        let input: &[u8] = b"\x1b[>1 u\x1b[>1>u\x1b[1u\x1b[=1;1u\x1b[?u";
        assert_eq!(run(&mut g, &[input]), input);
        assert_eq!(g.depth(), 0);
    }

    #[test]
    fn controls_inside_a_push_keep_their_order() {
        let mut g = KittyGuard::default();
        assert_eq!(run(&mut g, &[b"\x1b[>\x071u"]), b"\x1b[>\x071u");
        assert_eq!(g.depth(), 1);
    }

    #[test]
    fn reset_and_alternate_screen() {
        let mut g = KittyGuard::default();
        run(&mut g, &[b"\x1b[>1u\x1b[>1u"]);
        run(&mut g, &[b"\x1b[?1049h\x1b[>1u"]);
        assert_eq!(g.depth(), 1, "alternate screen has its own stack");
        run(&mut g, &[b"\x1b[?1049l"]);
        assert_eq!(g.depth(), 2);
        run(&mut g, &[b"\x1bc"]);
        assert_eq!(g.depth(), 0);
    }

    #[test]
    fn overlong_push_is_dropped() {
        let mut g = KittyGuard::default();
        let mut input = b"\x1b[>".to_vec();
        input.extend(std::iter::repeat_n(b'0', 500));
        input.extend_from_slice(b"1uok");
        assert_eq!(run(&mut g, &[&input]), b"ok");
    }
}
