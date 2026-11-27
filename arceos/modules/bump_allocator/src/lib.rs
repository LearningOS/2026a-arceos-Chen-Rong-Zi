#![no_std]

use allocator::{AllocError, BaseAllocator, ByteAllocator, PageAllocator};

/// Early memory allocator
/// Use it before formal bytes-allocator and pages-allocator can work!
/// This is a double-end memory range:
/// - Alloc bytes forward
/// - Alloc pages backward
///
/// [ bytes-used | avail-area | pages-used ]
/// |            | -->    <-- |            |
/// start       b_pos        p_pos       end
///
/// For bytes area, 'count' records number of allocations.
/// When it goes down to ZERO, free bytes-used area.
/// For pages area, it will never be freed!
///
pub struct EarlyAllocator<const SIZE: usize> {
    start: usize,
    end: usize,
    b_pos: usize,
    p_pos: usize,
    count: usize,
}

impl<const SIZE: usize> EarlyAllocator<SIZE> {
    pub const fn new() -> Self {
        assert!(SIZE.is_power_of_two());
        Self {
            start: 0,
            end: 0,
            b_pos: 0,
            p_pos: 0,
            count: 0,
        }
    }
}

impl<const SIZE: usize> BaseAllocator for EarlyAllocator<SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        let end = start.checked_add(size).expect("memory region overflows");
        let end = (end & !(SIZE - 1)).max(start);
        self.start = start;
        self.end = end;
        self.b_pos = start;
        self.p_pos = end;
        self.count = 0;
    }

    fn add_memory(&mut self, _start: usize, _size: usize) -> allocator::AllocResult {
        Err(AllocError::NoMemory)
    }
}

impl<const SIZE: usize> ByteAllocator for EarlyAllocator<SIZE> {
    fn alloc(
        &mut self,
        layout: core::alloc::Layout,
    ) -> allocator::AllocResult<core::ptr::NonNull<u8>> {
        if layout.size() == 0 {
            return Err(AllocError::InvalidParam);
        }
        let pos = self
            .b_pos
            .checked_add(layout.align() - 1)
            .ok_or(AllocError::NoMemory)?
            & !(layout.align() - 1);
        let end = pos.checked_add(layout.size()).ok_or(AllocError::NoMemory)?;
        if end > self.p_pos {
            return Err(AllocError::NoMemory);
        }
        let ptr = core::ptr::NonNull::new(pos as *mut u8).ok_or(AllocError::NoMemory)?;
        self.b_pos = end;
        self.count += 1;
        Ok(ptr)
    }

    fn dealloc(&mut self, _pos: core::ptr::NonNull<u8>, _layout: core::alloc::Layout) {
        self.count -= 1;
        if self.count == 0 {
            self.b_pos = self.start;
        }
    }

    fn total_bytes(&self) -> usize {
        self.p_pos - self.start
    }

    fn used_bytes(&self) -> usize {
        self.b_pos - self.start
    }

    fn available_bytes(&self) -> usize {
        self.p_pos - self.b_pos
    }
}

impl<const SIZE: usize> PageAllocator for EarlyAllocator<SIZE> {
    const PAGE_SIZE: usize = SIZE;

    fn alloc_pages(
        &mut self,
        num_pages: usize,
        align_pow2: usize,
    ) -> allocator::AllocResult<usize> {
        if num_pages == 0 || !align_pow2.is_power_of_two() || align_pow2 < SIZE {
            return Err(AllocError::InvalidParam);
        }
        let size = num_pages.checked_mul(SIZE).ok_or(AllocError::NoMemory)?;
        let pos = self.p_pos.checked_sub(size).ok_or(AllocError::NoMemory)? & !(align_pow2 - 1);
        if pos < self.b_pos {
            return Err(AllocError::NoMemory);
        }
        self.p_pos = pos;
        Ok(pos)
    }

    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {}

    fn total_pages(&self) -> usize {
        (self.end - self.b_pos) / SIZE
    }

    fn used_pages(&self) -> usize {
        (self.end - self.p_pos) / SIZE
    }

    fn available_pages(&self) -> usize {
        (self.p_pos - self.b_pos) / SIZE
    }
}
