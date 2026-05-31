//! `NullProvider` — the empty source a fresh document holds.
//!
//! Faithful port of `src/providers/null_provider.h`. `size()` is 0 and `read`
//! always fails; the empty `name()` makes the command row show `<Select Source>`.

use super::Provider;

/// `class NullProvider : public Provider` (`null_provider.h:6-12`).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullProvider;

impl Provider for NullProvider {
    fn size(&self) -> i32 {
        0
    }
    fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
        false
    }
}
