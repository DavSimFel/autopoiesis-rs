use std::time::{SystemTime, UNIX_EPOCH};

/// Return a UTC timestamp in `YYYY-MM-DDTHH:MM:SSZ` format.
pub(crate) fn utc_timestamp() -> String {
    const SECS_PER_MINUTE: i64 = 60;
    const SECS_PER_HOUR: i64 = 3_600;
    const SECS_PER_DAY: i64 = 86_400;

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let mut days = duration / SECS_PER_DAY;
    let mut rem = duration % SECS_PER_DAY;

    let hour = rem / SECS_PER_HOUR;
    rem %= SECS_PER_HOUR;
    let minute = rem / SECS_PER_MINUTE;
    let second = rem % SECS_PER_MINUTE;

    // 719_468 is the number of days from year 0 to the Unix epoch (1970-01-01) in the
    // proleptic Gregorian calendar; 146_097 is the number of days per 400-year cycle.
    days += 719_468;
    let era = if days >= 0 { days / 146_097 } else { (days - 146_096) / 146_097 };
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + (if month <= 2 { 1 } else { 0 });

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        hour,
        minute,
        second
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn utc_timestamp_represents_current_utc_time() {
        let value = utc_timestamp();
        let bytes: Vec<u8> = value.bytes().collect();
        assert_eq!(bytes.len(), 20);
        assert_eq!(&value[4..5], "-");
        assert_eq!(&value[7..8], "-");
        assert_eq!(&value[10..11], "T");
        assert_eq!(&value[13..14], ":");
        assert_eq!(&value[16..17], ":");
        assert_eq!(&value[19..20], "Z");
        assert!(value[..4].chars().all(|ch| ch.is_ascii_digit()));
        assert!(value[5..7].chars().all(|ch| ch.is_ascii_digit()));
        assert!(value[8..10].chars().all(|ch| ch.is_ascii_digit()));
        assert!(value[11..13].chars().all(|ch| ch.is_ascii_digit()));
        assert!(value[14..16].chars().all(|ch| ch.is_ascii_digit()));
        assert!(value[17..19].chars().all(|ch| ch.is_ascii_digit()));

        let year: i64 = value[0..4].parse().expect("year must parse as integer");
        let month: i64 = value[5..7].parse().expect("month must parse as integer");
        let day: i64 = value[8..10].parse().expect("day must parse as integer");
        let hour: i64 = value[11..13].parse().expect("hour must parse as integer");
        let minute: i64 = value[14..16].parse().expect("minute must parse as integer");
        let second: i64 = value[17..19].parse().expect("second must parse as integer");

        assert!(year >= 2026);

        let days = {
            let adj_year = year - if month <= 2 { 1 } else { 0 };
            let era = adj_year.div_euclid(400);
            let yoe = adj_year - era * 400;
            let month_index = if month > 2 { month - 3 } else { month + 9 };
            let doy = (153 * month_index + 2) / 5 + day - 1;
            let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
            era * 146_097 + doe - 719_468
        };
        let timestamp_secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_secs() as i64;

        let delta = (timestamp_secs - now).abs();
        assert!(delta <= 5);
    }
}
