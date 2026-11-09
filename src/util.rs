//! Small helpers: time conversion and human-readable sizes.

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (m as u64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

/// (year, month, day, hour, min, sec) in UTC.
pub fn civil_from_unix(t: i64) -> (i32, i32, i32, i32, i32, i32) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i32;
    let y = if m <= 2 { y + 1 } else { y } as i32;
    (y, m, d, (secs / 3600) as i32, ((secs % 3600) / 60) as i32, (secs % 60) as i32)
}

/// Interpret broken-down *local* time (as stored in DOS/ZIP/RAR headers) as a Unix timestamp.
pub fn unix_from_local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    #[cfg(unix)]
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = y - 1900;
        tm.tm_mon = mo as i32 - 1;
        tm.tm_mday = d as i32;
        tm.tm_hour = h as i32;
        tm.tm_min = mi as i32;
        tm.tm_sec = s as i32;
        tm.tm_isdst = -1;
        let t = libc::mktime(&mut tm);
        if t != -1 {
            return t as i64;
        }
    }
    days_from_civil(y as i64, mo, d) * 86400 + (h * 3600 + mi * 60 + s) as i64
}

/// Decode a packed DOS date/time (high 16 bits date, low 16 bits time).
pub fn unix_from_dos(dt: u32) -> i64 {
    let date = (dt >> 16) as u16;
    let time = dt as u16;
    let y = 1980 + (date >> 9) as i32;
    let mo = ((date >> 5) & 0xF).max(1) as u32;
    let d = (date & 0x1F).max(1) as u32;
    unix_from_local(y, mo, d, (time >> 11) as u32, ((time >> 5) & 0x3F) as u32, ((time & 0x1F) * 2) as u32)
}

pub fn format_time(t: Option<i64>) -> String {
    match t {
        None => "                ".into(),
        Some(t) => {
            let (y, m, d, h, mi, _) = civil_from_unix(t);
            format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
        }
    }
}

pub fn human(n: u64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

pub fn mode_string(kind: crate::entry::EntryKind, mode: Option<u32>) -> String {
    let m = mode.unwrap_or(if kind == crate::entry::EntryKind::Dir { 0o755 } else { 0o644 });
    let mut s = String::with_capacity(10);
    s.push(kind.as_char());
    for shift in [6, 3, 0] {
        let b = (m >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}
