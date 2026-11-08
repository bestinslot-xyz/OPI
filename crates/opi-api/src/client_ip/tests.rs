use super::client_ip;

const XFF: &str = " 1.1.1.1 , ,2.2.2.2,  3.3.3.3  ";

#[test]
fn no_trusted_proxy_answers_the_socket_address() {
    assert_eq!(client_ip("127.0.0.1", Some(XFF), 0), "127.0.0.1");
    assert_eq!(client_ip("127.0.0.1", None, 0), "127.0.0.1");
}

#[test]
fn trusted_hops_walk_x_forwarded_for_from_the_right() {
    // Express with `trust proxy` = 1, 2 and 5 answered these.
    assert_eq!(client_ip("127.0.0.1", Some(XFF), 1), "3.3.3.3");
    assert_eq!(client_ip("127.0.0.1", Some(XFF), 2), "2.2.2.2");
    assert_eq!(client_ip("127.0.0.1", Some(XFF), 5), "1.1.1.1");
    assert_eq!(client_ip("127.0.0.1", None, 2), "127.0.0.1");
}

#[test]
fn x_forwarded_for_entries_are_taken_as_they_are() {
    // the forwarded module trims spaces only and keeps inner text
    assert_eq!(client_ip("::1", Some("a b,\tc"), 1), "\tc");
    assert_eq!(client_ip("::1", Some("a b,\tc"), 2), "a b");
    assert_eq!(client_ip("::1", Some(","), 1), "::1");
}
