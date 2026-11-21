/// Formatea milisegundos como `m:ss` o `h:mm:ss`.
pub fn format_time(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(59_999), "0:59");
        assert_eq!(format_time(61_000), "1:01");
        assert_eq!(format_time(3_725_000), "1:02:05");
    }
}
