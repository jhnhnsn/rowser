//! Browser and Profile discovery on the Local machine (spec: Browsers).
//!
//! Every root is a plain value in [`Roots`], so tests point discovery at temp dirs.

use std::path::{Path, PathBuf};

use serde::Serialize;

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
    /// The user's home (Firefox's `~/.mozilla/firefox` on Linux).
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
}

/// The browsers found by one scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Browsers {
    pub list: Vec<Browser>,
}

#[derive(Clone, Copy)]
enum Family {
    Chromium,
    Firefox,
    None,
}

struct Known {
    name: &'static str,
    app: &'static str,
    bundle_id: &'static str,
    desktop_ids: &'static [&'static str],
    family: Family,
    mac_profiles: &'static str,
    linux_profiles: &'static str,
}

const KNOWN: &[Known] = &[
    Known {
        name: "safari",
        app: "Safari",
        bundle_id: "com.apple.Safari",
        desktop_ids: &[],
        family: Family::None,
        mac_profiles: "",
        linux_profiles: "",
    },
    Known {
        name: "chrome",
        app: "Google Chrome",
        bundle_id: "com.google.Chrome",
        desktop_ids: &["google-chrome", "google-chrome-stable", "com.google.Chrome"],
        family: Family::Chromium,
        mac_profiles: "Google/Chrome",
        linux_profiles: "google-chrome",
    },
    Known {
        name: "firefox",
        app: "Firefox",
        bundle_id: "org.mozilla.firefox",
        desktop_ids: &["firefox", "org.mozilla.firefox", "firefox-esr"],
        family: Family::Firefox,
        mac_profiles: "Firefox",
        linux_profiles: ".mozilla/firefox",
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
        family: Family::Chromium,
        mac_profiles: "Microsoft Edge",
        linux_profiles: "microsoft-edge",
    },
    Known {
        name: "brave",
        app: "Brave Browser",
        bundle_id: "com.brave.Browser",
        desktop_ids: &["brave-browser", "com.brave.Browser"],
        family: Family::Chromium,
        mac_profiles: "BraveSoftware/Brave-Browser",
        linux_profiles: "BraveSoftware/Brave-Browser",
    },
    Known {
        name: "vivaldi",
        app: "Vivaldi",
        bundle_id: "com.vivaldi.Vivaldi",
        desktop_ids: &["vivaldi-stable", "vivaldi", "com.vivaldi.Vivaldi"],
        family: Family::Chromium,
        mac_profiles: "Vivaldi",
        linux_profiles: "vivaldi",
    },
    Known {
        name: "chromium",
        app: "Chromium",
        bundle_id: "org.chromium.Chromium",
        desktop_ids: &["chromium", "chromium-browser", "org.chromium.Chromium"],
        family: Family::Chromium,
        mac_profiles: "Chromium",
        linux_profiles: "chromium",
    },
    Known {
        name: "arc",
        app: "Arc",
        bundle_id: "company.thebrowser.Browser",
        desktop_ids: &[],
        family: Family::None,
        mac_profiles: "",
        linux_profiles: "",
    },
];

impl Browsers {
    /// Finds the browsers and their Profiles under `roots`. Unreadable or malformed files
    /// give no Profiles; they never fail the scan.
    pub fn scan(roots: &Roots) -> Browsers {
        let list = match roots.os {
            Os::MacOs => scan_mac(roots),
            Os::Linux => scan_linux(roots),
        };
        Browsers { list }
    }

    /// One line per browser: `chrome  Google Chrome  profiles: Work (Profile 2)`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for b in &self.list {
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
            profiles: profiles(k.family, &roots.config_dir.join(k.mac_profiles)),
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
        paths.sort();
        for path in paths {
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".desktop"))
            else {
                continue;
            };
            if id == "rowser" || seen.iter().any(|s| s == id) {
                continue;
            }
            seen.push(id.to_string());
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(entry) = desktop_entry(&text) else {
                continue;
            };
            let known = KNOWN.iter().find(|k| k.desktop_ids.contains(&id));
            let profiles = match known {
                Some(k) if matches!(k.family, Family::Firefox) => {
                    profiles(k.family, &roots.home.join(k.linux_profiles))
                }
                Some(k) => profiles(k.family, &roots.config_dir.join(k.linux_profiles)),
                None => Vec::new(),
            };
            list.push(Browser {
                name: known.map_or(id, |k| k.name).to_string(),
                display_name: entry.unwrap_or_else(|| id.to_string()),
                bundle_id: None,
                desktop_id: Some(format!("{id}.desktop")),
                profiles,
            });
        }
    }
    list
}

/// `Some(Name)` when the `[Desktop Entry]` group's `MimeType` has `x-scheme-handler/https`.
fn desktop_entry(text: &str) -> Option<Option<String>> {
    let mut in_entry = false;
    let mut https = false;
    let mut name = None;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("MimeType=") {
            https = v.split(';').any(|m| m.trim() == "x-scheme-handler/https");
        } else if let Some(v) = line.strip_prefix("Name=") {
            name = Some(v.trim().to_string());
        }
    }
    https.then_some(name)
}

fn profiles(family: Family, dir: &Path) -> Vec<Profile> {
    let mut list = match family {
        Family::Chromium => chromium_profiles(dir),
        Family::Firefox => firefox_profiles(dir),
        Family::None => Vec::new(),
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
    let mut list = Vec::new();
    let mut current: Option<(Option<String>, Option<String>)> = None;
    let mut flush = |c: &mut Option<(Option<String>, Option<String>)>| {
        if let Some((Some(name), Some(path))) = c.take() {
            list.push(Profile { name, dir: path });
        }
    };
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            flush(&mut current);
            if line.starts_with("[Profile") {
                current = Some((None, None));
            }
        } else if let Some((name, path)) = current.as_mut() {
            if let Some(v) = line.strip_prefix("Name=") {
                *name = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("Path=") {
                *path = Some(v.to_string());
            }
        }
    }
    flush(&mut current);
    list
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
    } else if found.list.is_empty() {
        eprintln!("rowser: no browsers found");
    } else {
        print!("{}", found.to_text());
    }
    std::process::ExitCode::SUCCESS
}
