//! Pagination helpers.

/// Compute a 0-based offset from a 1-based page number and a page size.
///
/// ```
/// use oracle_plugin::utils::pagination::offset_for;
/// assert_eq!(offset_for(1, 100), 0);
/// assert_eq!(offset_for(3, 25), 50);
/// assert_eq!(offset_for(0, 10), 0); // page 0 is clamped to page 1
/// ```
pub fn offset_for(page: u64, page_size: u64) -> u64 {
    page.max(1).saturating_sub(1).saturating_mul(page_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_page_has_no_offset() {
        assert_eq!(offset_for(1, 100), 0);
    }

    #[test]
    fn later_pages() {
        assert_eq!(offset_for(3, 25), 50);
        assert_eq!(offset_for(2, 50), 50);
    }

    #[test]
    fn page_zero_is_treated_as_one() {
        assert_eq!(offset_for(0, 10), 0);
    }
}
