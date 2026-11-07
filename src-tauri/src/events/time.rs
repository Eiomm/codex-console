use std::time::{SystemTime, UNIX_EPOCH};
pub fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64 }
// Gregorian calendar conversion (civil date <-> days since Unix epoch).
fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yo = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    era * 146097 + yo * 365 + yo / 4 - yo / 100 + (153 * mp + 2) / 5 + d - 1 - 719468
}
pub fn date_path(day: i64) -> String {
    let z = day + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yo = (doe - doe/1460 + doe/36524 - doe/146096)/365;
    let y = yo + era*400;
    let doy = doe - (365*yo + yo/4 - yo/100);
    let mp = (5*doy+2)/153;
    let d = doy - (153*mp+2)/5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    format!("{:04}/{:02}/{:02}", y + i64::from(m <= 2), m, d)
}
pub fn parse_ms(s: &str) -> Option<u64> {
    // Strict ASCII RFC3339; reject malformed dates instead of guessing or panicking.
    if !s.is_ascii() || s.len() < 20 || &s[4..5] != "-" || &s[7..8] != "-" || &s[10..11] != "T" || &s[13..14] != ":" || &s[16..17] != ":" { return None; }
    let number = |a, b| {
        let part = s.get(a..b)?;
        if !part.bytes().all(|b| b.is_ascii_digit()) { return None; }
        part.parse::<i64>().ok()
    };
    let (y,m,d,h,min,sec) = (number(0,4)?,number(5,7)?,number(8,10)?,number(11,13)?,number(14,16)?,number(17,19)?);
    let leap = y%4 == 0 && (y%100 != 0 || y%400 == 0);
    let md = match m { 2 => if leap {29} else {28}, 4|6|9|11 => 30, 1|3|5|7|8|10|12 => 31, _ => return None };
    if !(1..=md).contains(&d) || h > 23 || min > 59 || sec > 59 { return None; }
    let mut pos = 19;
    let mut ms = 0;
    if s.as_bytes().get(pos) == Some(&b'.') {
        pos += 1;
        let start = pos;
        while s.as_bytes().get(pos).is_some_and(u8::is_ascii_digit) { pos += 1; }
        if start == pos { return None; }
        for (i,b) in s.as_bytes()[start..pos].iter().take(3).enumerate() { ms += (b-b'0') as i64 * [100,10,1][i]; }
    }
    let zone = s.get(pos..)?;
    let offset = if zone == "Z" { 0 } else if zone.len() == 6 && matches!(zone.as_bytes()[0], b'+'|b'-') && &zone[3..4] == ":" {
        if !zone[1..3].bytes().chain(zone[4..6].bytes()).all(|b| b.is_ascii_digit()) { return None; }
        let zh = zone[1..3].parse::<i64>().ok()?;
        let zm = zone[4..6].parse::<i64>().ok()?;
        if zh > 23 || zm > 59 { return None; }
        (zh*3600+zm*60) * if zone.starts_with('-') {-1} else {1}
    } else { return None; };
    u64::try_from((days(y,m,d)*86400 + h*3600 + min*60 + sec - offset)*1000 + ms).ok()
}
