/// Splits `host:port` at the last colon.
pub fn addr(s: &str) -> Option<(&str, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    Some((host, port.parse().ok()?))
}
