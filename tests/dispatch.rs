use rowser::cli::{parse_from, Command};

fn open_url(argv: &[&str]) -> String {
    match parse_from(argv.iter().copied()).expect("parse") {
        Command::Open { url } => url,
        other => panic!("expected open, got {other:?}"),
    }
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
    assert!(parse_from(["something-else", "https://example.com/"]).is_err());
}

#[test]
fn every_stub_subcommand_parses() {
    let cases: &[(&[&str], &str)] = &[
        (&["rowser", "daemon"], "daemon"),
        (&["rowser", "daemon", "install"], "daemon install"),
        (&["rowser", "daemon", "uninstall"], "daemon uninstall"),
        (&["rowser", "forward", "8080"], "forward"),
        (&["rowser", "remote", "install", "box"], "remote install"),
        (&["rowser", "__keeper"], "__keeper"),
    ];
    for (argv, name) in cases {
        let cmd = parse_from(argv.iter().copied()).expect("parse");
        assert_eq!(cmd.name(), *name, "{argv:?}");
    }
}

#[test]
fn ssh_passes_its_arguments_through() {
    match parse_from(["rowser", "ssh", "-p", "22", "host"]).expect("parse") {
        Command::Ssh { args } => assert_eq!(args, ["-p", "22", "host"]),
        other => panic!("expected ssh, got {other:?}"),
    }
}
