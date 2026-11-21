//! Browser and Profile discovery on the Local machine (spec: Browsers).
//!
//! Every root is a plain value in [`Roots`], so tests point discovery at temp dirs.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::Target;

#[cfg(test)]
mod tests;

/// Which OS layout to scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
}

/// The directories discovery reads.
#[derive(Debug, Clone)]
pub struct Roots {
    pub os: Os,
    /// macOS: `/Applications` and `~/Applications`.
    pub app_dirs: Vec<PathBuf>,
    /// Linux: `$XDG_DATA_HOME` then each `$XDG_DATA_DIRS` entry (not yet `applications`).
    pub data_dirs: Vec<PathBuf>,
    /// The user's home (Linux: `~/.mozilla/firefox`, Flatpak `~/.var/app`, Snap `~/snap`).
    pub home: PathBuf,
    /// Where Chromium-family profile dirs live: `~/Library/Application Support` on macOS,
    /// `$XDG_CONFIG_HOME` on Linux.
    pub config_dir: PathBuf,
}

impl Roots {
    /// The roots for this machine, from `HOME` and the XDG variables.
    pub fn from_env() -> Roots {
        Roots::from_vars(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
    }

    fn from_vars(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Roots {
        let home = PathBuf::from(var("HOME").unwrap_or_default());
        if cfg!(target_os = "macos") {
            Roots {
                os: Os::MacOs,
                app_dirs: vec!["/Applications".into(), home.join("Applications")],
                data_dirs: Vec::new(),
                config_dir: home.join("Library/Application Support"),
                home,
            }
        } else {
            let data_home = var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"));
            let dirs = var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            let mut data_dirs = vec![data_home];
            data_dirs.extend(std::env::split_paths(&dirs).filter(|p| !p.as_os_str().is_empty()));
            Roots {
                os: Os::Linux,
                app_dirs: Vec::new(),
                data_dirs,
                config_dir: var("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config")),
                home,
            }
        }
    }
}

/// One of a browser's Profiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    /// Display name (`Work`; Firefox: `Name`).
    pub name: String,
    /// Chromium: the profile folder (`Profile 2`); Firefox: `Path` from `profiles.ini`.
    pub dir: String,
}

/// A browser found on the Local machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Browser {
    /// Short name (`chrome`), used in Targets.
    pub name: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_id: Option<String>,
    pub profiles: Vec<Profile>,
    /// Linux: the desktop file's `Exec` line.
    #[serde(skip)]
    pub exec: Option<String>,
}

/// The browsers found by one scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Browsers {
    pub all: Vec<Browser>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Chromium,
    Firefox,
}

/// Where a known browser keeps its Profiles.
struct ProfileDirs {
    family: Family,
    /// Under `~/Library/Application Support`.
    mac: &'static str,
    /// Chromium: under `$XDG_CONFIG_HOME`. Firefox: unused (see [`linux_profile_dirs`]).
    linux: &'static str,
}

struct Known {
    name: &'static str,
    app: &'static str,
    bundle_id: &'static str,
    /// Native, Flatpak (reverse-DNS) and Snap (`<snap>_<app>`) desktop ids.
    desktop_ids: &'static [&'static str],
    /// `None`: no Profiles in rowser (Safari, Arc).
    profiles: Option<ProfileDirs>,
}

const fn chromium(mac: &'static str, linux: &'static str) -> Option<ProfileDirs> {
    Some(ProfileDirs {
        family: Family::Chromium,
        mac,
        linux,
    })
}

const KNOWN: &[Known] = &[
    Known {
        name: "safari",
        app: "Safari",
        bundle_id: "com.apple.Safari",
        desktop_ids: &[],
        profiles: None,
    },
    Known {
        name: "chrome",
        app: "Google Chrome",
        bundle_id: "com.google.Chrome",
        desktop_ids: &["google-chrome", "google-chrome-stable", "com.google.Chrome"],
        profiles: chromium("Google/Chrome", "google-chrome"),
    },
    Known {
        name: "firefox",
        app: "Firefox",
        bundle_id: "org.mozilla.firefox",
        desktop_ids: &[
            "firefox",
            "org.mozilla.firefox",
            "firefox-esr",
            "firefox_firefox",
        ],
        profiles: Some(ProfileDirs {
            family: Family::Firefox,
            mac: "Firefox",
            linux: "",
        }),
    },
    Known {
        name: "edge",
        app: "Microsoft Edge",
        bundle_id: "com.microsoft.edgemac",
        desktop_ids: &[
            "microsoft-edge",
            "microsoft-edge-stable",
            "com.microsoft.Edge",
        ],
        profiles: chromium("Microsoft Edge", "microsoft-edge"),
    },
    Known {
        name: "brave",
        app: "Brave Browser",
        bundle_id: "com.brave.Browser",
        desktop_ids: &["brave-browser", "com.brave.Browser", "brave_brave"],
        profiles: chromium("BraveSoftware/Brave-Browser", "BraveSoftware/Brave-Browser"),
    },
    Known {
        name: "vivaldi",
        app: "Vivaldi",
        bundle_id: "com.vivaldi.Vivaldi",
        desktop_ids: &["vivaldi-stable", "vivaldi", "com.vivaldi.Vivaldi"],
        profiles: chromium("Vivaldi", "vivaldi"),
    },
    Known {
        name: "chromium",
        app: "Chromium",
        bundle_id: "org.chromium.Chromium",
        desktop_ids: &[
            "chromium",
            "chromium-browser",
            "org.chromium.Chromium",
            "chromium_chromium",
        ],
        profiles: chromium("Chromium", "chromium"),
    },
    Known {
        name: "arc",
        app: "Arc",
        bundle_id: "company.thebrowser.Browser",
        desktop_ids: &[],
        profiles: None,
    },
];

impl Browsers {
    /// Finds the browsers and their Profiles under `roots`. Unreadable or malformed files
    /// give no Profiles; they never fail the scan.
    pub fn scan(roots: &Roots) -> Browsers {
        let all = match roots.os {
            Os::MacOs => scan_mac(roots),
            Os::Linux => scan_linux(roots),
        };
        Browsers { all }
    }

    /// One line per browser: `chrome  Google Chrome  profiles: Personal (Default)`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for b in &self.all {
            out.push_str(&b.name);
            out.push_str("  ");
            out.push_str(&b.display_name);
            if !b.profiles.is_empty() {
                let profiles: Vec<String> = b
                    .profiles
                    .iter()
                    .map(|p| format!("{} ({})", p.name, p.dir))
                    .collect();
                out.push_str("  profiles: ");
                out.push_str(&profiles.join(", "));
            }
            out.push('\n');
        }
        out
    }
}

fn scan_mac(roots: &Roots) -> Vec<Browser> {
    KNOWN
        .iter()
        .filter(|k| {
            roots
                .app_dirs
                .iter()
                .any(|d| d.join(format!("{}.app", k.app)).is_dir())
        })
        .map(|k| Browser {
            name: k.name.into(),
            display_name: k.app.into(),
            bundle_id: Some(k.bundle_id.into()),
            desktop_id: None,
            profiles: k.profiles.as_ref().map_or_else(Vec::new, |p| {
                read_profiles(p.family, &roots.config_dir.join(p.mac))
            }),
            exec: None,
        })
        .collect()
}

fn scan_linux(roots: &Roots) -> Vec<Browser> {
    let mut seen: Vec<String> = Vec::new();
    let mut list = Vec::new();
    for data in &roots.data_dirs {
        let Ok(entries) = std::fs::read_dir(data.join("applications")) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        // By desktop id, so `firefox` comes before `firefox-esr` and gets the short name.
        paths.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));
        for path in paths {
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".desktop"))
            else {
                continue;
            };
            // The first file for a desktop id wins, also when it is hidden (XDG: deleted).
            if id == "rowser" || seen.iter().any(|s| s == id) {
                continue;
            }
            seen.push(id.to_string());
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let entry = DesktopEntry::parse(&text);
            if !entry.https || entry.hidden {
                continue;
            }
            // A short name belongs to the first desktop id that claims it; a later one
            // (`google-chrome-stable` after `google-chrome`) keeps its desktop id.
            let known = KNOWN
                .iter()
                .find(|k| k.desktop_ids.contains(&id))
                .filter(|k| list.iter().all(|b: &Browser| b.name != k.name));
            let profiles = known
                .and_then(|k| k.profiles.as_ref())
                .map_or_else(Vec::new, |p| {
                    linux_profile_dirs(roots, id, p)
                        .iter()
                        .map(|dir| read_profiles(p.family, dir))
                        .find(|found| !found.is_empty())
                        .unwrap_or_default()
                });
            list.push(Browser {
                name: known.map_or(id, |k| k.name).to_string(),
                display_name: entry.name.unwrap_or_else(|| id.to_string()),
                bundle_id: None,
                desktop_id: Some(format!("{id}.desktop")),
                profiles,
                exec: entry.exec,
            });
        }
    }
    list
}

/// The dirs that can hold a Linux browser's Profiles, first match wins. Flatpak (`a.b.C`)
/// and Snap (`snap_app`) browsers keep them in their sandbox under the home.
fn linux_profile_dirs(roots: &Roots, desktop_id: &str, dirs: &ProfileDirs) -> Vec<PathBuf> {
    let (home, config) = if let Some((snap, _)) = desktop_id.split_once('_') {
        let base = roots.home.join("snap").join(snap).join("common");
        (base.clone(), base)
    } else if desktop_id.contains('.') {
        let base = roots.home.join(".var/app").join(desktop_id);
        (base.clone(), base.join("config"))
    } else {
        (roots.home.clone(), roots.config_dir.clone())
    };
    match dirs.family {
        Family::Chromium => vec![config.join(dirs.linux)],
        // Legacy first: Firefox 147+ keeps `~/.mozilla/firefox` when it exists and uses
        // `$XDG_CONFIG_HOME/mozilla/firefox` only when it does not.
        Family::Firefox => vec![
            home.join(".mozilla/firefox"),
            config.join("mozilla/firefox"),
        ],
    }
}

/// The `[Desktop Entry]` group keys discovery reads.
#[derive(Default)]
struct DesktopEntry {
    /// `MimeType` has `x-scheme-handler/https`.
    https: bool,
    /// `Hidden=true`: XDG treats the file as deleted.
    hidden: bool,
    name: Option<String>,
    exec: Option<String>,
}

impl DesktopEntry {
    fn parse(text: &str) -> DesktopEntry {
        let mut entry = DesktopEntry::default();
        let mut in_entry = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
            } else if !in_entry {
            } else if let Some(v) = line.strip_prefix("MimeType=") {
                entry.https = v.split(';').any(|m| m.trim() == "x-scheme-handler/https");
            } else if let Some(v) = line.strip_prefix("Hidden=") {
                entry.hidden = v.trim() == "true";
            } else if let Some(v) = line.strip_prefix("Name=") {
                entry.name = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Exec=") {
                entry.exec = Some(v.trim().to_string());
            }
        }
        entry
    }
}

/// The Profiles in one browser's Profile dir, sorted by display name.
fn read_profiles(family: Family, dir: &Path) -> Vec<Profile> {
    let mut list = match family {
        Family::Chromium => chromium_profiles(dir),
        Family::Firefox => firefox_profiles(dir),
    };
    list.sort_by(|a, b| a.name.cmp(&b.name).then(a.dir.cmp(&b.dir)));
    list
}

fn chromium_profiles(dir: &Path) -> Vec<Profile> {
    let Ok(text) = std::fs::read_to_string(dir.join("Local State")) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(cache) = state
        .get("profile")
        .and_then(|p| p.get("info_cache"))
        .and_then(|c| c.as_object())
    else {
        return Vec::new();
    };
    cache
        .iter()
        .map(|(folder, info)| Profile {
            name: info
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(folder)
                .to_string(),
            dir: folder.clone(),
        })
        .collect()
}

fn firefox_profiles(dir: &Path) -> Vec<Profile> {
    let Ok(text) = std::fs::read_to_string(dir.join("profiles.ini")) else {
        return Vec::new();
    };
    /// One `[Profile<n>]` section; it gives a Profile when it has both keys.
    #[derive(Default)]
    struct Section {
        name: Option<String>,
        path: Option<String>,
    }
    let mut sections: Vec<Section> = Vec::new();
    let mut in_profile = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_profile = line.starts_with("[Profile");
            if in_profile {
                sections.push(Section::default());
            }
        } else if let (true, Some(s)) = (in_profile, sections.last_mut()) {
            if let Some(v) = line.strip_prefix("Name=") {
                s.name = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("Path=") {
                s.path = Some(v.to_string());
            }
        }
    }
    sections
        .into_iter()
        .filter_map(|s| {
            Some(Profile {
                name: s.name?,
                dir: s.path?,
            })
        })
        .collect()
}

fn family_of(name: &str) -> Option<Family> {
    KNOWN
        .iter()
        .find(|k| k.name == name)
        .and_then(|k| k.profiles.as_ref())
        .map(|p| p.family)
}

impl Browsers {
    /// The browser and Profile a browser Target names. The profile is
    /// looked up by folder name first, then by display name. The error says
    /// what is missing.
    pub fn find(&self, target: &Target) -> Result<(&Browser, Option<&Profile>), String> {
        let Target::Browser { browser, profile } = target else {
            return Err(format!("`{target}` is not a browser target"));
        };
        let b = self
            .all
            .iter()
            .find(|b| &b.name == browser)
            .ok_or_else(|| format!("browser `{browser}` is not installed"))?;
        let Some(want) = profile else {
            return Ok((b, None));
        };
        let p = b
            .profiles
            .iter()
            .find(|p| &p.dir == want)
            .or_else(|| b.profiles.iter().find(|p| &p.name == want))
            .ok_or_else(|| format!("browser `{browser}` has no profile `{want}`"))?;
        Ok((b, Some(p)))
    }

    /// The Target in the form rowser writes: the profile as its folder name
    /// (Firefox: its `-P` name).
    pub fn normalize(&self, target: &Target) -> Result<Target, String> {
        let (b, p) = self.find(target)?;
        Ok(Target::Browser {
            browser: b.name.clone(),
            profile: p.map(|p| profile_arg(b, p).to_string()),
        })
    }

    /// The argv that opens `url` in a browser Target.
    pub fn command(&self, os: Os, target: &Target, url: &str) -> Result<Vec<String>, String> {
        let (b, p) = self.find(target)?;
        argv(os, b, p, url)
    }
}

/// The value that names `p` on the command line: the folder for Chromium,
/// the display name for Firefox (`-P <name>`).
fn profile_arg<'a>(b: &Browser, p: &'a Profile) -> &'a str {
    match family_of(&b.name) {
        Some(Family::Firefox) => &p.name,
        _ => &p.dir,
    }
}

/// The argv that opens `url` in `browser` (and `profile`) on `os`. Pure:
/// it runs nothing and never goes through a shell.
///
/// macOS: `open [-n] -b <bundle id> <url> [--args <profile args>]`.
/// Linux: the desktop file's `Exec`, with the field codes replaced; the
/// profile arguments go just before the URL.
pub fn argv(
    os: Os,
    browser: &Browser,
    profile: Option<&Profile>,
    url: &str,
) -> Result<Vec<String>, String> {
    let family = family_of(&browser.name);
    let profile_args: Vec<String> = match (family, profile) {
        (Some(Family::Chromium), Some(p)) => vec![format!("--profile-directory={}", p.dir)],
        (Some(Family::Firefox), Some(p)) => vec!["-P".into(), p.name.clone()],
        _ => vec![],
    };
    match os {
        Os::MacOs => {
            let id = browser
                .bundle_id
                .as_deref()
                .ok_or_else(|| format!("browser `{}` has no bundle id", browser.name))?;
            let mut v = vec!["open".to_string()];
            if family == Some(Family::Chromium) && !profile_args.is_empty() {
                v.push("-n".into());
            }
            v.extend(["-b".to_string(), id.to_string(), url.to_string()]);
            if !profile_args.is_empty() {
                v.push("--args".into());
                v.extend(profile_args);
            }
            Ok(v)
        }
        Os::Linux => {
            let exec = browser
                .exec
                .as_deref()
                .ok_or_else(|| format!("browser `{}` has no Exec line", browser.name))?;
            let words = split_exec(exec)?;
            let mut v: Vec<String> = Vec::new();
            let mut placed = false;
            for w in words {
                match w.as_str() {
                    "%u" | "%U" | "%f" | "%F" => {
                        if !placed {
                            v.extend(profile_args.iter().cloned());
                            v.push(url.to_string());
                            placed = true;
                        }
                    }
                    "%i" | "%c" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" | "@@u" | "@@" => {
                    }
                    _ => v.push(w.replace("%%", "%")),
                }
            }
            if v.is_empty() {
                return Err(format!("browser `{}` has an empty Exec line", browser.name));
            }
            if !placed {
                v.extend(profile_args);
                v.push(url.to_string());
            }
            Ok(v)
        }
    }
}

/// Splits an `Exec` value into words: spaces separate words, double quotes
/// group them, and a backslash in quotes escapes the next character.
fn split_exec(exec: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            '\\' if quoted => match chars.next() {
                Some(n) => cur.push(n),
                None => return Err("the Exec line ends in a backslash".into()),
            },
            ' ' | '\t' if !quoted => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            _ => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if quoted {
        return Err("the Exec line has an unclosed quote".into());
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// `rowser browsers [--json]`.
pub fn browsers_main(json: bool) -> std::process::ExitCode {
    let found = Browsers::scan(&Roots::from_env());
    if json {
        match serde_json::to_string_pretty(&found) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("rowser browsers: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    } else if found.all.is_empty() {
        eprintln!("rowser: no browsers found");
    } else {
        print!("{}", found.to_text());
    }
    std::process::ExitCode::SUCCESS
}
