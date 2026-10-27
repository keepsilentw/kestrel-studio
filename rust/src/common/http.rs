// Shared parsing for route/query parameters, which arrive as strings.

/// Returns the value as a positive integer, or `None` when it is not one.
pub fn read_positive_int(value: Option<&str>) -> Option<i64> {
    let parsed = value?.trim().parse::<i64>().ok()?;
    (parsed > 0).then_some(parsed)
}

/// Returns the value as a non-negative integer, or `None` when it is not one.
/// Stream cursors use this: 0 is a legal "from the beginning" position, while an
/// id parsed out of a header or query must still be a whole number.
pub fn read_non_negative_int(value: Option<&str>) -> Option<i64> {
    let parsed = value?.trim().parse::<i64>().ok()?;
    (parsed >= 0).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_accept_zero_but_ids_do_not() {
        assert_eq!(read_positive_int(Some("0")), None);
        assert_eq!(read_positive_int(Some("7")), Some(7));
        assert_eq!(read_positive_int(Some("1.5")), None);
        assert_eq!(read_non_negative_int(Some("0")), Some(0));
        assert_eq!(read_non_negative_int(Some("-1")), None);
        assert_eq!(read_non_negative_int(None), None);
    }
}
