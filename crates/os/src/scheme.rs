//! The colour scheme of the desktop, for `prefers-color-scheme`. Read once when a window starts.
//!
//! `BARE_COLOR_SCHEME=dark|light` wins. On Linux the GTK settings (`gtk-4.0/settings.ini`, then `gtk-3.0`) give
//! `gtk-application-prefer-dark-theme`. Elsewhere the scheme is light.
//! Gap: the XDG desktop portal (`org.freedesktop.appearance` `color-scheme`) is not read: it needs D-Bus. A change while
//! the browser runs is not seen.

#[cfg(test)]
#[path = "scheme_tests.rs"]
mod tests;

/// Does the desktop prefer a dark scheme?
pub fn prefers_dark() -> bool {
    if let Some(dark) = scheme_from_env(std::env::var("BARE_COLOR_SCHEME").ok().as_deref()) {
        return dark;
    }
    if !cfg!(target_os = "linux") {
        return false;
    }
    let path = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    let Some(config) = path("XDG_CONFIG_HOME").or_else(|| path("HOME").map(|h| h.join(".config"))) else { return false };
    ["gtk-4.0", "gtk-3.0"].iter().find_map(|d| std::fs::read_to_string(config.join(d).join("settings.ini")).ok()).is_some_and(|t| dark_in_ini(&t))
}

fn scheme_from_env(v: Option<&str>) -> Option<bool> {
    match v?.to_ascii_lowercase().as_str() {
        "dark" => Some(true),
        "light" => Some(false),
        _ => None,
    }
}

fn dark_in_ini(text: &str) -> bool {
    text.lines().filter_map(|l| l.split_once('=')).any(|(k, v)| k.trim() == "gtk-application-prefer-dark-theme" && matches!(v.trim(), "1" | "true"))
}
