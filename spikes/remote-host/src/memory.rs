//! Wasm linear memory never shrinks: the post-capture size is its high water,
//! not live Rust allocations or allocator heap peak.
pub fn pages() -> Option<usize> {
    #[cfg(target_arch = "wasm32")]
    {
        Some(core::arch::wasm32::memory_size::<0>())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}
