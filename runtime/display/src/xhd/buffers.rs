// SPDX-License-Identifier: MIT
//! What each of the layer's buffers lacks (2026-11-22). Surface::lock copies back into the buffer it hands out only
//! `mDirtyRegion - dirty` from the last posted one, and it keeps mDirtyRegion by subtracting the handed-out buffer's
//! own last dirty rectangle (AOSP libs/gui/Surface.cpp): right for two buffers, but with three or more a change posted
//! in another buffer is lost from it wherever this buffer's old rectangle covered it, and that buffer then shows the
//! area as it was frames ago. With direct frames only the changed tiles were asked for, so a widget or a page that
//! changed showed an older picture of itself for a frame and then the right one ("when the UI changes it glitches and
//! updates", 2026-11-22). Now the whole buffer is locked (so Surface copies nothing back) and the helper fills what
//! that buffer lacks itself, from the snapshot: the union of what every post changed since the buffer was last
//! filled, and what this post changes. Buffers are told apart by the address the lock maps them at; one not seen yet
//! (or forgotten: more than BUFFERS) is filled whole.
use super::Rect;

pub const BUFFERS: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Known {
    bits: usize,
    stale: Rect,
    used: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Buffers {
    pub width: u32,
    pub height: u32,
    known: Vec<Known>,
    clock: u64,
    /// How many were forgotten for a newer one (the stats line's "forgotten").
    pub forgotten: u64,
}

impl Buffers {
    /// All of them unknown (new buffers).
    pub fn new(width: u32, height: u32) -> Buffers {
        Buffers { width, height, known: Vec::with_capacity(BUFFERS), clock: 0, forgotten: 0 }
    }

    /// The same, in place (a format fallback: new buffers).
    pub fn reset(&mut self, width: u32, height: u32) {
        *self = Buffers::new(width, height);
    }

    /// How many buffers are known.
    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    /// `bits`: the buffer's address (0 is no buffer: never known).
    fn find(&self, bits: usize) -> Option<usize> {
        if bits == 0 {
            return None;
        }
        self.known.iter().position(|k| k.bits == bits)
    }

    /// The rectangle to fill in the buffer at `bits`: what it lacks, and `changed` (what this post changes).
    pub fn fill(&self, bits: usize, changed: &Rect) -> Rect {
        match self.find(bits) {
            // not seen (or forgotten): what it holds is not known
            None => Rect::whole(self.width, self.height),
            Some(i) => {
                let mut r = self.known[i].stale;
                r.union(changed);
                r
            }
        }
    }

    /// The buffer at `bits` was filled and posted, changing `changed` against the last post: the others lack that now.
    pub fn posted(&mut self, bits: usize, changed: &Rect) {
        let found = self.find(bits);
        for (j, k) in self.known.iter_mut().enumerate() {
            if Some(j) != found {
                k.stale.union(changed);
            }
        }
        let i = found.unwrap_or_else(|| {
            let fresh = Known { bits, stale: Rect::EMPTY, used: 0 };
            if self.known.len() < BUFFERS {
                self.known.push(fresh);
                self.known.len() - 1
            } else {
                // the one posted longest ago is forgotten: filled whole next time
                let oldest = (0..self.known.len()).min_by_key(|&j| self.known[j].used).unwrap_or(0);
                self.forgotten += 1;
                self.known[oldest] = fresh;
                oldest
            }
        });
        self.clock += 1;
        self.known[i].stale = Rect::EMPTY;
        self.known[i].used = self.clock;
    }
}
