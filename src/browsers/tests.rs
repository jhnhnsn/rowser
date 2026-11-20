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
    b.list.iter().map(|b| b.name.as_str()).collect()
}

#[test]
fn macos_finds_known_app_bundles_in_both_application_dirs() {
    let t = tmp_dir("m");
    std::fs::create_dir_all(t.0.join("Applications/Safari.app")).unwrap();
    std::fs::create_dir_all(t.0.join("home/Applications/Arc.app")).unwrap();
    std::fs::create_dir_all(t.0.join("Applications/Unknown.app")).unwrap();
    let found = Browsers::scan(&mac_roots(&t.0));
    assert_eq!(names(&found), ["safari", "arc"]);
    assert_eq!(found.list[0].bundle_id.as_deref(), Some("com.apple.Safari"));
    assert_eq!(
        found.list[1].bundle_id.as_deref(),
        Some("company.thebrowser.Browser")
    );
    assert!(found.list.iter().all(|b| b.profiles.is_empty()));
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
        found.list[0].desktop_id.as_deref(),
        Some("google-chrome.desktop")
    );
    assert_eq!(found.list[1].display_name, "Odd Browser");
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
    assert_eq!(found.list.len(), 1);
    assert_eq!(found.list[0].display_name, "My Firefox");
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
