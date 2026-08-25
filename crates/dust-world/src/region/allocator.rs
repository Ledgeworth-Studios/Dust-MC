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
