#[cfg(test)]
mod tests;

/// Express's `request.ip` for `trust proxy` set to a hop count, as
/// proxy-addr computes it: the addresses are the socket's followed by the
/// X-Forwarded-For entries from right to left; hop `i` is trusted when
/// `i < trusted_hops`, and the answer is the address after the last trusted
/// hop (or the leftmost entry when every hop is trusted).
pub fn client_ip(socket: &str, x_forwarded_for: Option<&str>, trusted_hops: usize) -> String {
    let forwarded = parse_forwarded(x_forwarded_for.unwrap_or(""));
    let mut addrs = Vec::with_capacity(forwarded.len() + 1);
    addrs.push(socket);
    addrs.extend(forwarded);
    let mut last = addrs.len() - 1;
    for i in 0..addrs.len() - 1 {
        if i >= trusted_hops {
            last = i;
            break;
        }
    }
    addrs[last].to_string()
}

/// The `forwarded` module's X-Forwarded-For parse: entries split at commas
/// from the right, with leading and trailing spaces (only U+0020) dropped
/// and empty entries skipped.
fn parse_forwarded(header: &str) -> Vec<&str> {
    let bytes = header.as_bytes();
    let mut list = Vec::new();
    let mut end = bytes.len();
    let mut start = bytes.len();
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b' ' => {
                if start == end {
                    start = i;
                    end = i;
                }
            }
            b',' => {
                if start != end {
                    list.push(&header[start..end]);
                }
                start = i;
                end = i;
            }
            _ => start = i,
        }
    }
    if start != end {
        list.push(&header[start..end]);
    }
    list
}
