use super::*;

struct TmpDir(PathBuf);

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tmp_dir(tag: &str) -> TmpDir {
    let dir = std::env::temp_dir().join(format!("rw.b{tag}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TmpDir(dir)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn mac_roots(t: &Path) -> Roots {
    Roots {
        os: Os::MacOs,
        app_dirs: vec![t.join("Applications"), t.join("home/Applications")],
        data_dirs: Vec::new(),
        home: t.join("home"),
        config_dir: t.join("home/Library/Application Support"),
    }
}

fn linux_roots(t: &Path) -> Roots {
    Roots {
        os: Os::Linux,
        app_dirs: Vec::new(),
        data_dirs: vec![t.join("data-home"), t.join("usr/share")],
        home: t.join("home"),
        config_dir: t.join("home/.config"),
    }
}

const TWO_PROFILES: &str = r#"{"profile":{"info_cache":{
    "Default":{"name":"Personal"},"Profile 2":{"name":"Work"}}}}"#;

fn desktop(name: &str, mime: &str) -> String {
    format!("[Desktop Entry]\nType=Application\nName={name}\nMimeType={mime}\n")
}

fn names(b: &Browsers) -> Vec<&str> {
    b.all.iter().map(|b| b.name.as_str()).collect()
}

#[test]
fn macos_finds_known_app_bundles_in_both_application_dirs() {
    let t = tmp_dir("m");
    std::fs::create_dir_all(t.0.join("Applications/Safari.app")).unwrap();
    std::fs::create_dir_all(t.0.join("home/Applications/Arc.app")).unwrap();
    std::fs::create_dir_all(t.0.join("Applications/Unknown.app")).unwrap();
    let found = Browsers::scan(&mac_roots(&t.0));
    assert_eq!(names(&found), ["safari", "arc"]);
    assert_eq!(found.all[0].bundle_id.as_deref(), Some("com.apple.Safari"));
    assert_eq!(
        found.all[1].bundle_id.as_deref(),
        Some("company.thebrowser.Browser")
    );
    assert!(found.all.iter().all(|b| b.profiles.is_empty()));
}

#[test]
fn macos_chrome_lists_two_profiles_from_local_state() {
    let t = tmp_dir("c");
    std::fs::create_dir_all(t.0.join("Applications/Google Chrome.app")).unwrap();
    write(
        &t.0.join("home/Library/Application Support/Google/Chrome/Local State"),
        TWO_PROFILES,
    );
    let found = Browsers::scan(&mac_roots(&t.0));
    assert_eq!(
        found.to_text(),
        "chrome  Google Chrome  profiles: Personal (Default), Work (Profile 2)\n"
    );
}

#[test]
fn malformed_local_state_lists_the_browser_with_no_profiles() {
    let t = tmp_dir("x");
    std::fs::create_dir_all(t.0.join("Applications/Brave Browser.app")).unwrap();
    write(
        &t.0.join("home/Library/Application Support/BraveSoftware/Brave-Browser/Local State"),
        "{not json",
    );
    let found = Browsers::scan(&mac_roots(&t.0));
    assert_eq!(found.to_text(), "brave  Brave Browser\n");
}

#[test]
fn linux_lists_https_handlers_and_skips_others_and_rowser() {
    let t = tmp_dir("l");
    let apps = t.0.join("usr/share/applications");
    write(
        &apps.join("google-chrome.desktop"),
        &desktop(
            "Google Chrome",
            "text/html;x-scheme-handler/http;x-scheme-handler/https;",
        ),
    );
    write(
        &apps.join("org.example.Odd.desktop"),
        &desktop("Odd Browser", "x-scheme-handler/https"),
    );
    write(&apps.join("gimp.desktop"), &desktop("GIMP", "image/png;"));
    write(
        &apps.join("rowser.desktop"),
        &desktop("rowser", "x-scheme-handler/https;"),
    );
    let found = Browsers::scan(&linux_roots(&t.0));
    assert_eq!(names(&found), ["chrome", "org.example.Odd"]);
    assert_eq!(
        found.all[0].desktop_id.as_deref(),
        Some("google-chrome.desktop")
    );
    assert_eq!(found.all[1].display_name, "Odd Browser");
}

#[test]
fn linux_data_home_wins_over_data_dirs_for_one_desktop_id() {
    let t = tmp_dir("h");
    write(
        &t.0.join("data-home/applications/firefox.desktop"),
        &desktop("My Firefox", "x-scheme-handler/https;"),
    );
    write(
        &t.0.join("usr/share/applications/firefox.desktop"),
        &desktop("Firefox", "x-scheme-handler/https;"),
    );
    let found = Browsers::scan(&linux_roots(&t.0));
    assert_eq!(found.all.len(), 1);
    assert_eq!(found.all[0].display_name, "My Firefox");
}

#[test]
fn linux_firefox_lists_two_profiles_from_profiles_ini() {
    let t = tmp_dir("f");
    write(
        &t.0.join("usr/share/applications/firefox.desktop"),
        &desktop("Firefox", "x-scheme-handler/https;"),
    );
    write(
        &t.0.join("home/.mozilla/firefox/profiles.ini"),
        "[Install4F96D1932A9F858E]\nDefault=abc.default-release\n\n\
         [Profile1]\nName=work\nIsRelative=1\nPath=xyz.work\n\n\
         [Profile0]\nName=default-release\nIsRelative=1\nPath=abc.default-release\n\n\
         [General]\nStartWithLastProfile=1\n",
    );
    let found = Browsers::scan(&linux_roots(&t.0));
    assert_eq!(
        found.to_text(),
        "firefox  Firefox  profiles: default-release (abc.default-release), work (xyz.work)\n"
    );
}

const ONE_FIREFOX_PROFILE: &str = "[Profile0]\nName=default\nIsRelative=1\nPath=abc.default\n";

fn firefox_desktop(t: &Path, id: &str) {
    write(
        &t.join(format!("usr/share/applications/{id}.desktop")),
        &desktop("Firefox", "x-scheme-handler/https;"),
    );
}

#[test]
fn linux_firefox_reads_profiles_ini_under_xdg_config_home() {
    let t = tmp_dir("fx");
    firefox_desktop(&t.0, "firefox");
    write(
        &t.0.join("home/.config/mozilla/firefox/profiles.ini"),
        ONE_FIREFOX_PROFILE,
    );
    let found = Browsers::scan(&linux_roots(&t.0));
    assert_eq!(
        found.to_text(),
        "firefox  Firefox  profiles: default (abc.default)\n"
    );
}

#[test]
fn linux_firefox_prefers_the_legacy_dir_when_both_exist() {
    let t = tmp_dir("fl");
    firefox_desktop(&t.0, "firefox");
    write(
        &t.0.join("home/.config/mozilla/firefox/profiles.ini"),
        "[Profile0]\nName=new\nPath=n.new\n",
    );
    write(
        &t.0.join("home/.mozilla/firefox/profiles.ini"),
        ONE_FIREFOX_PROFILE,
    );
    let found = Browsers::scan(&linux_roots(&t.0));
    assert_eq!(
        found.to_text(),
        "firefox  Firefox  profiles: default (abc.default)\n"
    );
}

#[test]
fn linux_snap_and_flatpak_ids_map_to_known_names_and_sandbox_profiles() {
    let t = tmp_dir("sf");
    firefox_desktop(&t.0, "firefox_firefox");
    write(
        &t.0.join("home/snap/firefox/common/.mozilla/firefox/profiles.ini"),
        ONE_FIREFOX_PROFILE,
    );
    write(
        &t.0.join("usr/share/applications/com.google.Chrome.desktop"),
        &desktop("Google Chrome", "x-scheme-handler/https;"),
    );
    write(
        &t.0.join("home/.var/app/com.google.Chrome/config/google-chrome/Local State"),
        TWO_PROFILES,
    );
    write(
        &t.0.join("usr/share/applications/chromium_chromium.desktop"),
        &desktop("Chromium", "x-scheme-handler/https;"),
    );
    let found = Browsers::scan(&linux_roots(&t.0));
    assert_eq!(
        found.to_text(),
        "chromium  Chromium\n\
         chrome  Google Chrome  profiles: Personal (Default), Work (Profile 2)\n\
         firefox  Firefox  profiles: default (abc.default)\n"
    );
}

#[test]
fn linux_second_desktop_id_for_one_short_name_keeps_its_desktop_id() {
    let t = tmp_dir("du");
    firefox_desktop(&t.0, "firefox-esr");
    firefox_desktop(&t.0, "firefox");
    write(
        &t.0.join("usr/share/applications/google-chrome-stable.desktop"),
        &desktop("Google Chrome", "x-scheme-handler/https;"),
    );
    write(
        &t.0.join("data-home/applications/google-chrome.desktop"),
        &desktop("Google Chrome", "x-scheme-handler/https;"),
    );
    let found = Browsers::scan(&linux_roots(&t.0));
    assert_eq!(
        names(&found),
        ["chrome", "firefox", "firefox-esr", "google-chrome-stable"]
    );
}

#[test]
fn linux_skips_hidden_desktop_files_and_their_lower_copies() {
    let t = tmp_dir("hd");
    write(
        &t.0.join("data-home/applications/firefox.desktop"),
        "[Desktop Entry]\nName=Firefox\nMimeType=x-scheme-handler/https;\nHidden=true\n",
    );
    firefox_desktop(&t.0, "firefox");
    let found = Browsers::scan(&linux_roots(&t.0));
    assert!(found.all.is_empty());
}

#[test]
fn linux_chrome_reads_local_state_under_the_config_dir() {
    let t = tmp_dir("g");
    write(
        &t.0.join("usr/share/applications/google-chrome.desktop"),
        &desktop("Google Chrome", "x-scheme-handler/https;"),
    );
    write(
        &t.0.join("home/.config/google-chrome/Local State"),
        TWO_PROFILES,
    );
    let found = Browsers::scan(&linux_roots(&t.0));
    let json = serde_json::to_value(&found).unwrap();
    assert_eq!(
        json,
        serde_json::json!([{
            "name": "chrome",
            "display_name": "Google Chrome",
            "desktop_id": "google-chrome.desktop",
            "profiles": [
                {"name": "Personal", "dir": "Default"},
                {"name": "Work", "dir": "Profile 2"}
            ]
        }])
    );
}

#[test]
fn linux_roots_from_vars_use_xdg_defaults() {
    let roots = Roots::from_vars(|k| (k == "HOME").then(|| "/h".into()));
    if roots.os == Os::Linux {
        assert_eq!(
            roots.data_dirs,
            [
                PathBuf::from("/h/.local/share"),
                "/usr/local/share".into(),
                "/usr/share".into()
            ]
        );
        assert_eq!(roots.config_dir, PathBuf::from("/h/.config"));
    }
}

fn browser(name: &str, bundle: &str, exec: &str, profiles: &[(&str, &str)]) -> Browser {
    Browser {
        name: name.into(),
        display_name: name.into(),
        bundle_id: Some(bundle.into()),
        desktop_id: None,
        profiles: profiles
            .iter()
            .map(|(n, d)| Profile {
                name: n.to_string(),
                dir: d.to_string(),
            })
            .collect(),
        exec: Some(exec.into()),
    }
}

fn set() -> Browsers {
    Browsers {
        all: vec![
            browser(
                "chrome",
                "com.google.Chrome",
                "/usr/bin/google-chrome-stable %U",
                &[("Personal", "Default"), ("Work", "Profile 2")],
            ),
            browser(
                "firefox",
                "org.mozilla.firefox",
                "/usr/lib/firefox/firefox %u",
                &[("Dev", "abc.dev")],
            ),
            browser("safari", "com.apple.Safari", "", &[]),
        ],
        os: None,
    }
}

fn t(s: &str) -> Target {
    s.parse().unwrap()
}

const U: &str = "https://e.com/a?b=1&c=%20";

#[test]
fn macos_argv_per_browser_kind() {
    let b = set();
    let argv = |s| b.command(Os::MacOs, &t(s), U).unwrap();
    assert_eq!(
        argv("chrome:Work"),
        [
            "open",
            "-n",
            "-b",
            "com.google.Chrome",
            U,
            "--args",
            "--profile-directory=Profile 2"
        ]
    );
    assert_eq!(argv("chrome"), ["open", "-b", "com.google.Chrome", U]);
    assert_eq!(
        argv("firefox:abc.dev"),
        [
            "open",
            "-b",
            "org.mozilla.firefox",
            U,
            "--args",
            "-P",
            "Dev"
        ]
    );
    assert_eq!(argv("safari"), ["open", "-b", "com.apple.Safari", U]);
}

#[test]
fn linux_argv_puts_the_profile_before_the_url_in_exec() {
    let b = set();
    let argv = |s| b.command(Os::Linux, &t(s), U).unwrap();
    assert_eq!(
        argv("chrome:Profile 2"),
        [
            "/usr/bin/google-chrome-stable",
            "--profile-directory=Profile 2",
            U
        ]
    );
    assert_eq!(argv("chrome"), ["/usr/bin/google-chrome-stable", U]);
    assert_eq!(
        argv("firefox:Dev"),
        ["/usr/lib/firefox/firefox", "-P", "Dev", U]
    );
}

#[test]
fn linux_argv_handles_field_codes_quotes_and_flatpak_markers() {
    let mut b = browser("chrome", "x", "", &[("Work", "Profile 2")]);
    let run = |b: &Browser, p| argv(Os::Linux, b, p, U).unwrap();
    b.exec = Some(
        "/usr/bin/flatpak run --command=chrome com.google.Chrome @@u %U @@ --icon %i %c %k".into(),
    );
    assert_eq!(
        run(&b, None),
        [
            "/usr/bin/flatpak",
            "run",
            "--command=chrome",
            "com.google.Chrome",
            U,
            "--icon"
        ]
    );
    b.exec = Some(r#""/opt/My Browser/bin" --x="a \"q\"" 100%%"#.into());
    assert_eq!(
        run(&b, Some(&b.profiles[0].clone())),
        [
            "/opt/My Browser/bin",
            "--x=a \"q\"",
            "100%",
            "--profile-directory=Profile 2",
            U
        ]
    );
    b.exec = Some("\"unclosed".into());
    assert!(argv(Os::Linux, &b, None, U).is_err());
}

#[test]
fn linux_argv_decodes_desktop_entry_string_escapes() {
    let mut b = browser("chrome", "x", "", &[]);
    // The escapes decode first, so a decoded space still separates words
    // unless it is in quotes.
    b.exec = Some(r#""/opt/My\sBrowser/bin" --a="x\ty" --b=\\s a\sb %u"#.into());
    assert_eq!(
        argv(Os::Linux, &b, None, U).unwrap(),
        ["/opt/My Browser/bin", "--a=x\ty", r"--b=\s", "a", "b", U]
    );
    b.exec = Some(r#""/opt/a\\\\b" --n=1\n2 --r=\r"#.into());
    assert_eq!(
        argv(Os::Linux, &b, None, U).unwrap(),
        [r"/opt/a\b", "--n=1\n2", "--r=\r", U]
    );
}

#[test]
fn a_scanned_browser_with_no_usable_exec_is_not_available() {
    let mut b = set();
    b.os = Some(Os::Linux);
    b.all[0].exec = None;
    assert_eq!(
        b.normalize(&t("chrome")).unwrap_err(),
        "browser `chrome` has no Exec line"
    );
    b.all[0].exec = Some("\"unclosed".into());
    assert!(b.normalize(&t("chrome")).is_err());
    assert_eq!(b.normalize(&t("firefox")).unwrap(), t("firefox"));
}

#[test]
fn lookup_tries_the_folder_name_then_the_display_name() {
    let b = set();
    assert_eq!(
        b.normalize(&t("chrome:Work")).unwrap(),
        t("chrome:Profile 2")
    );
    assert_eq!(
        b.normalize(&t("chrome:Default")).unwrap(),
        t("chrome:Default")
    );
    assert_eq!(
        b.normalize(&t("firefox:abc.dev")).unwrap(),
        t("firefox:abc.dev")
    );
    assert_eq!(
        b.normalize(&t("firefox:Dev")).unwrap(),
        t("firefox:abc.dev")
    );
    let mut folder_clash = set();
    folder_clash.all[0].profiles.push(Profile {
        name: "Profile 2".into(),
        dir: "Profile 9".into(),
    });
    assert_eq!(
        folder_clash.normalize(&t("chrome:Profile 2")).unwrap(),
        t("chrome:Profile 2")
    );
}

#[test]
fn a_missing_browser_or_profile_is_an_error_that_names_it() {
    let b = set();
    assert_eq!(
        b.normalize(&t("edge")).unwrap_err(),
        "browser `edge` is not installed"
    );
    assert_eq!(
        b.normalize(&t("chrome:Nope")).unwrap_err(),
        "browser `chrome` has no profile `Nope`"
    );
    assert!(b.normalize(&t("safari:Work")).is_err());
}

#[test]
fn linux_scan_keeps_the_exec_line() {
    let dir = tmp_dir("exec");
    write(
        &dir.0.join("usr/share/applications/firefox.desktop"),
        &format!(
            "{}Exec=firefox %u\n[Desktop Action new]\nExec=firefox --new-window %u\n",
            desktop("Firefox", "x-scheme-handler/https;")
        ),
    );
    let found = Browsers::scan(&linux_roots(&dir.0));
    assert_eq!(found.all[0].exec.as_deref(), Some("firefox %u"));
}
