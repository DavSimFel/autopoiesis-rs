pub(crate) use crate::time::utc_timestamp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_is_nonempty() {
        assert!(!utc_timestamp().is_empty());
    }
}
