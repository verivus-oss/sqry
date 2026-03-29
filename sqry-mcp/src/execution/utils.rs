//! Utility functions for MCP tool execution.
//!
//! This module provides common utility functions used across tool handlers,
//! including time conversion and pagination.

use std::time::{Duration, SystemTime};

use crate::pagination::encode_cursor;
use crate::tools::PaginationArgs;

/// Convert duration to milliseconds safely.
///
/// Durations beyond `u64::MAX` ms (~584 million years) are impossible; clamps to max.
pub(crate) fn duration_to_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

/// Get current time as epoch milliseconds.
pub(crate) fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(duration_to_ms)
        .unwrap_or(0)
}

/// Paginate a slice of items according to pagination arguments.
///
/// Returns a tuple of (paginated slice, optional next page cursor).
pub(crate) fn paginate<'a, T>(
    items: &'a [T],
    pagination: &PaginationArgs,
) -> (&'a [T], Option<String>) {
    if items.is_empty() {
        return (&[], None);
    }

    let start = pagination.offset.min(items.len());
    let end = (start + pagination.size).min(items.len());
    let next = if end < items.len() {
        Some(encode_cursor(end))
    } else {
        None
    };

    (&items[start..end], next)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== duration_to_ms tests ==========

    #[test]
    fn test_duration_to_ms_zero() {
        let duration = Duration::from_secs(0);
        assert_eq!(duration_to_ms(duration), 0);
    }

    #[test]
    fn test_duration_to_ms_one_second() {
        let duration = Duration::from_secs(1);
        assert_eq!(duration_to_ms(duration), 1000);
    }

    #[test]
    fn test_duration_to_ms_with_millis() {
        let duration = Duration::from_millis(1234);
        assert_eq!(duration_to_ms(duration), 1234);
    }

    #[test]
    fn test_duration_to_ms_one_hour() {
        let duration = Duration::from_secs(3600);
        assert_eq!(duration_to_ms(duration), 3_600_000);
    }

    #[test]
    fn test_duration_to_ms_with_nanos() {
        // 500 microseconds = 0.5 ms, should truncate to 0 when calling as_millis
        let duration = Duration::from_micros(500);
        assert_eq!(duration_to_ms(duration), 0);
    }

    #[test]
    fn test_duration_to_ms_large_value() {
        // 1 year in seconds
        let duration = Duration::from_secs(365 * 24 * 3600);
        assert_eq!(duration_to_ms(duration), 31_536_000_000);
    }

    // ========== current_epoch_ms tests ==========

    #[test]
    fn test_current_epoch_ms_is_reasonable() {
        let now = current_epoch_ms();
        // Should be after Jan 1, 2020 (1577836800000 ms)
        assert!(now > 1_577_836_800_000);
        // Should be before year 2100 (4102444800000 ms)
        assert!(now < 4_102_444_800_000);
    }

    // ========== paginate tests ==========

    #[test]
    fn test_paginate_empty_slice() {
        let items: Vec<i32> = vec![];
        let pagination = PaginationArgs {
            offset: 0,
            size: 10,
        };
        let (result, next) = paginate(&items, &pagination);
        assert!(result.is_empty());
        assert!(next.is_none());
    }

    #[test]
    fn test_paginate_single_page() {
        let items = vec![1, 2, 3, 4, 5];
        let pagination = PaginationArgs {
            offset: 0,
            size: 10,
        };
        let (result, next) = paginate(&items, &pagination);
        assert_eq!(result, &[1, 2, 3, 4, 5]);
        assert!(next.is_none());
    }

    #[test]
    fn test_paginate_first_page() {
        let items = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let pagination = PaginationArgs { offset: 0, size: 3 };
        let (result, next) = paginate(&items, &pagination);
        assert_eq!(result, &[1, 2, 3]);
        assert!(next.is_some());
    }

    #[test]
    fn test_paginate_middle_page() {
        let items = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let pagination = PaginationArgs { offset: 3, size: 3 };
        let (result, next) = paginate(&items, &pagination);
        assert_eq!(result, &[4, 5, 6]);
        assert!(next.is_some());
    }

    #[test]
    fn test_paginate_last_page() {
        let items = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let pagination = PaginationArgs { offset: 9, size: 3 };
        let (result, next) = paginate(&items, &pagination);
        assert_eq!(result, &[10]);
        assert!(next.is_none());
    }

    #[test]
    fn test_paginate_offset_beyond_items() {
        let items = vec![1, 2, 3];
        let pagination = PaginationArgs {
            offset: 100,
            size: 10,
        };
        let (result, next) = paginate(&items, &pagination);
        assert!(result.is_empty());
        assert!(next.is_none());
    }

    #[test]
    fn test_paginate_exact_page_boundary() {
        let items = vec![1, 2, 3, 4, 5, 6];
        let pagination = PaginationArgs { offset: 0, size: 3 };
        let (result, next) = paginate(&items, &pagination);
        assert_eq!(result, &[1, 2, 3]);
        assert!(next.is_some());

        // Second page
        let pagination2 = PaginationArgs { offset: 3, size: 3 };
        let (result2, next2) = paginate(&items, &pagination2);
        assert_eq!(result2, &[4, 5, 6]);
        assert!(next2.is_none());
    }

    #[test]
    fn test_paginate_size_one() {
        let items = vec![1, 2, 3];
        let pagination = PaginationArgs { offset: 0, size: 1 };
        let (result, next) = paginate(&items, &pagination);
        assert_eq!(result, &[1]);
        assert!(next.is_some());
    }

    #[test]
    fn test_paginate_with_strings() {
        let items = vec!["a", "b", "c", "d"];
        let pagination = PaginationArgs { offset: 1, size: 2 };
        let (result, next) = paginate(&items, &pagination);
        assert_eq!(result, &["b", "c"]);
        assert!(next.is_some());
    }
}
