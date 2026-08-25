//! Which sectors of a region file are in use.
//!
//! # Why a bitmap and not a list of free runs
//!
//! "Free list" describes the job and not the representation. A list of free
//! runs has to coalesce on every free — a run given back between two free runs
//! becomes one run, and forgetting to merge is how a file grows forever while
//! reporting plenty of space. A bitmap gets coalescing for nothing: adjacent
//! free sectors are adjacent zero bits, and a first-fit scan sees the merged
//! run without anything having merged it. Vanilla's `RegionBitmap` makes the
//! same choice. [`SectorAllocator::free_runs`] renders the free list on demand,
//! which is what tests want and what nothing on the write path needs.
//!
//! **What this does not catch:** it tracks sectors, not their contents. An
//! allocator that is perfectly consistent with itself and inconsistent with the
//! header describes a file whose chunks all read as garbage, and only the
//! validation at open time compares the two.

use crate::region::header::FIRST_DATA_SECTOR;

/// The sector map of one region file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectorAllocator {
    used: Vec<bool>,
}

/// A sector claimed twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectorTaken {
    pub sector: u32,
}

impl SectorAllocator {
    /// An allocator for a file of `sectors` sectors, with the header's two
    /// already spoken for.
    #[must_use]
    pub fn new(sectors: u64) -> Self {
        let sectors = sectors.max(u64::from(FIRST_DATA_SECTOR));
        let mut used = vec![false; usize::try_from(sectors).unwrap_or(usize::MAX)];
        for slot in used.iter_mut().take(FIRST_DATA_SECTOR as usize) {
            *slot = true;
        }
        Self { used }
    }

    /// How many sectors the file is currently understood to span.
    #[must_use]
    pub fn sectors(&self) -> u64 {
        self.used.len() as u64
    }

    /// How many are in use, the header's two included.
    #[must_use]
    pub fn used_sectors(&self) -> usize {
        self.used.iter().filter(|u| **u).count()
    }

    /// Mark an existing run as used, as reading a header does.
    ///
    /// Fails on the first sector that was already claimed, which is how two
    /// chunks whose ranges overlap are found.
    pub fn claim(&mut self, first: u32, count: u32) -> Result<(), SectorTaken> {
        self.grow_to(first as u64 + count as u64);
        for sector in first..first + count {
            if self.used[sector as usize] {
                return Err(SectorTaken { sector });
            }
            self.used[sector as usize] = true;
        }
        Ok(())
    }

    /// Find `count` free sectors, extending the file if there is no run long
    /// enough, and mark them used.
    ///
    /// First fit rather than best fit. A region file's runs are all a handful
    /// of sectors long and the table is 1024 entries; the difference between
    /// the two policies is not measurable, and first fit keeps a rewritten file
    /// packed towards the front, which is what makes a rewrite roughly the size
    /// of the original instead of a sparse image of it.
    pub fn allocate(&mut self, count: u32) -> u32 {
        assert!(count > 0, "a chunk occupies at least one sector");
        let count = count as usize;
        let mut run_start = None;
        for sector in FIRST_DATA_SECTOR as usize..self.used.len() {
            if self.used[sector] {
                run_start = None;
                continue;
            }
            let start = *run_start.get_or_insert(sector);
            if sector + 1 - start >= count {
                for slot in &mut self.used[start..start + count] {
                    *slot = true;
                }
                return start as u32;
            }
        }
        // Nothing long enough: extend past the end. The trailing free sectors,
        // if any, are the front of the new run rather than being abandoned.
        let tail = self.used.iter().rev().take_while(|u| !**u).count();
        let start = self.used.len() - tail;
        self.grow_to((start + count) as u64);
        for slot in &mut self.used[start..start + count] {
            *slot = true;
        }
        start as u32
    }

    /// Give a run back.
    ///
    /// Freeing sectors that are already free is allowed and does nothing: a
    /// caller that has just replaced a chunk frees the run the header used to
    /// point at, and that header may have been damaged in ways the open-time
    /// validation dropped rather than refused.
    pub fn free(&mut self, first: u32, count: u32) {
        for sector in first..first + count {
            if let Some(slot) = self.used.get_mut(sector as usize) {
                *slot = false;
            }
        }
    }

    /// Whether a run is entirely free.
    #[must_use]
    pub fn is_free(&self, first: u32, count: u32) -> bool {
        (first..first + count).all(|s| self.used.get(s as usize).is_none_or(|u| !*u))
    }

    /// The free list, as `(first_sector, length)` pairs in order.
    ///
    /// Trailing free sectors at the end of the file are included, because they
    /// are as reusable as any other run.
    #[must_use]
    pub fn free_runs(&self) -> Vec<(u32, u32)> {
        let mut runs = Vec::new();
        let mut start: Option<usize> = None;
        for sector in 0..self.used.len() {
            match (self.used[sector], start) {
                (false, None) => start = Some(sector),
                (true, Some(from)) => {
                    runs.push((from as u32, (sector - from) as u32));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(from) = start {
            runs.push((from as u32, (self.used.len() - from) as u32));
        }
        runs
    }

    fn grow_to(&mut self, sectors: u64) {
        if sectors > self.used.len() as u64 {
            self.used
                .resize(usize::try_from(sectors).unwrap_or(usize::MAX), false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_sectors_are_never_handed_out() {
        let mut allocator = SectorAllocator::new(0);
        assert_eq!(allocator.sectors(), 2);
        assert_eq!(allocator.used_sectors(), 2);
        assert_eq!(allocator.allocate(1), 2, "sectors 0 and 1 are the header");
    }

    #[test]
    fn a_freed_run_is_reused_before_the_file_grows() {
        let mut allocator = SectorAllocator::new(2);
        let a = allocator.allocate(3);
        let b = allocator.allocate(2);
        let c = allocator.allocate(1);
        assert_eq!((a, b, c), (2, 5, 7));
        assert_eq!(allocator.sectors(), 8);

        allocator.free(b, 2);
        assert!(allocator.is_free(5, 2));
        // A two-sector chunk fits the hole exactly.
        assert_eq!(allocator.allocate(2), 5);
        assert_eq!(allocator.sectors(), 8, "the file did not have to grow");
    }

    #[test]
    fn a_run_too_long_for_the_hole_goes_past_the_end() {
        let mut allocator = SectorAllocator::new(2);
        let a = allocator.allocate(2);
        let b = allocator.allocate(2);
        allocator.free(a, 2);
        // Three does not fit in the two-sector hole at 2.
        let c = allocator.allocate(3);
        assert_eq!(c, 6, "after the run at {b}");
        assert!(allocator.is_free(2, 2), "and the hole is still there");
    }

    #[test]
    fn adjacent_freed_runs_merge_without_anything_merging_them() {
        // The reason this is a bitmap. Three separate one-sector runs given
        // back become one three-sector run, and a free list of runs would have
        // to notice that.
        let mut allocator = SectorAllocator::new(2);
        let runs: Vec<u32> = (0..3).map(|_| allocator.allocate(1)).collect();
        for first in &runs {
            allocator.free(*first, 1);
        }
        assert_eq!(allocator.allocate(3), 2);
    }

    #[test]
    fn trailing_free_sectors_are_the_front_of_a_run_that_extends_the_file() {
        // A file ending in one free sector, asked for three, must use that one
        // and add two -- not abandon it and add three. Getting this wrong makes
        // a file grow by a sector on every rewrite of a chunk that grew.
        let mut allocator = SectorAllocator::new(2);
        let a = allocator.allocate(1);
        let b = allocator.allocate(1);
        assert_eq!((a, b), (2, 3));
        allocator.free(b, 1);
        assert_eq!(allocator.allocate(3), 3);
        assert_eq!(allocator.sectors(), 6);
    }

    #[test]
    fn claiming_a_sector_twice_names_it() {
        let mut allocator = SectorAllocator::new(10);
        allocator.claim(4, 3).expect("nothing held 4..7 yet");
        let taken = allocator.claim(6, 2).expect_err("6 is inside 4..7");
        assert_eq!(taken, SectorTaken { sector: 6 });
    }

    #[test]
    fn the_free_list_is_the_complement_of_what_is_used() {
        let mut allocator = SectorAllocator::new(10);
        allocator.claim(3, 2).expect("free");
        allocator.claim(7, 1).expect("free");
        assert_eq!(allocator.free_runs(), vec![(2, 1), (5, 2), (8, 2)]);
        assert_eq!(allocator.used_sectors(), 5);
    }

    #[test]
    fn freeing_something_already_free_changes_nothing() {
        // The lenient-open path frees runs from header entries it dropped, and
        // two of those entries may have pointed at the same sectors.
        let mut allocator = SectorAllocator::new(6);
        allocator.free(2, 2);
        allocator.free(2, 2);
        allocator.free(100, 4);
        assert_eq!(allocator.used_sectors(), 2);
        assert_eq!(allocator.sectors(), 6, "freeing past the end does not grow");
    }
}
