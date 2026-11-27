use rowser::cli::{parse_from, Command};

fn open_url(argv: &[&str]) -> String {
    match parse_from(argv.iter().copied()).expect("parse") {
        Command::Open { url } => url.to_string(),
        other => panic!("expected open, got {other:?}"),
    }
}

#[test]
fn open_rejects_a_malformed_url() {
    assert!(parse_from(["rowser", "open", "not a url"]).is_err());
    assert!(parse_from(["xdg-open", "http://[::1"]).is_err());
}

#[test]
fn browser_names_in_argv0_act_as_open() {
    for name in ["xdg-open", "sensible-browser", "www-browser"] {
        let argv0 = format!("/home/u/.local/bin/{name}");
        assert_eq!(
            open_url(&[&argv0, "https://example.com/a"]),
            "https://example.com/a"
        );
    }
}

#[test]
fn unknown_argv0_falls_back_to_subcommand_parsing() {
    assert_eq!(
        open_url(&["/usr/bin/rowser", "open", "https://example.com/"]),
        "https://example.com/"
    );
    assert!(matches!(
        parse_from(["something-else", "doctor"]).expect("parse"),
        Command::Doctor
    ));
    assert!(parse_from(["something-else", "not-a-url"]).is_err());
}

#[test]
fn a_url_as_first_argument_acts_as_open() {
    // BROWSER=rowser: a program runs `rowser <url>`.
    assert_eq!(
        open_url(&["/usr/bin/rowser", "https://example.com/a?b=c"]),
        "https://example.com/a?b=c"
    );
    assert_eq!(
        open_url(&["rowser", "http://localhost:8080/cb"]),
        "http://localhost:8080/cb"
    );
}

#[test]
fn a_non_http_url_as_first_argument_acts_as_open() {
    // The Daemon, not the Shim, applies the scheme allowlist.
    assert_eq!(open_url(&["rowser", "ftp://x/"]), "ftp://x/");
    assert_eq!(open_url(&["rowser", "ssh://host"]), "ssh://host");
}

#[test]
fn non_url_first_arguments_keep_subcommand_parsing() {
    assert!(matches!(
        parse_from(["rowser", "doctor"]).expect("parse"),
        Command::Doctor
    ));
    assert!(parse_from(["rowser", "not a url"]).is_err());
    assert!(parse_from(["rowser", "://x"]).is_err());
    assert!(parse_from(["rowser", "--help"]).is_err());
}

#[test]
fn every_stub_subcommand_parses() {
    let cases: &[(&[&str], &str)] = &[
        (&["rowser", "daemon"], "daemon"),
        (&["rowser", "daemon", "install"], "daemon install"),
        (&["rowser", "daemon", "uninstall"], "daemon uninstall"),
        (&["rowser", "forward", "8080"], "forward"),
        (&["rowser", "remote", "install", "box"], "remote install"),
    ];
    for (argv, name) in cases {
        let cmd = parse_from(argv.iter().copied()).expect("parse");
        assert_eq!(cmd.label(), *name, "{argv:?}");
    }
}

#[test]
fn forward_rejects_privileged_ports() {
    for port in ["0", "80", "1023"] {
        assert!(parse_from(["rowser", "forward", port]).is_err(), "{port}");
    }
    for port in ["1024", "65535"] {
        assert!(parse_from(["rowser", "forward", port]).is_ok(), "{port}");
    }
}

#[test]
fn daemon_loopback_accepts_only_loopback_addresses() {
    for ip in ["0.0.0.0", "::", "192.168.1.2"] {
        let err = parse_from(["rowser", "daemon", "--loopback", ip]).unwrap_err();
        assert!(
            err.to_string().contains("is not a loopback address"),
            "{err}"
        );
    }
    for ip in ["127.0.0.1", "::1"] {
        assert!(
            parse_from(["rowser", "daemon", "--loopback", ip]).is_ok(),
            "{ip}"
        );
    }
}

#[test]
fn ssh_passes_its_arguments_through() {
    match parse_from(["rowser", "ssh", "-p", "22", "host"]).expect("parse") {
        Command::Ssh { args } => assert_eq!(args, ["-p", "22", "host"]),
        other => panic!("expected ssh, got {other:?}"),
    }
}

#[test]
fn ssh_keeps_a_double_dash_and_help_flags() {
    for args in [
        &["--", "-G", "localhost"][..],
        &["-h"][..],
        &["--help", "host"][..],
        &["-V"][..],
    ] {
        let mut argv = vec!["rowser", "ssh"];
        argv.extend_from_slice(args);
        match parse_from(argv).expect("parse") {
            Command::Ssh { args: got } => assert_eq!(got, args, "{args:?}"),
            other => panic!("expected ssh, got {other:?}"),
        }
    }
}

#[cfg(unix)]
#[test]
fn ssh_passes_non_utf8_arguments_unchanged() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let raw = OsString::from_vec(vec![b'h', 0xff, b'x']);
    let argv: Vec<OsString> = vec!["rowser".into(), "ssh".into(), raw.clone()];
    match parse_from(argv).expect("parse") {
        Command::Ssh { args } => assert_eq!(args, [raw]),
        other => panic!("expected ssh, got {other:?}"),
    }
}
