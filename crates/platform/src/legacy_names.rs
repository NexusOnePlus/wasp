//! The one-time move from the names this project shipped under before Wasp.
//!
//! Wasp is a fork of Velo, which began as a fork of a client called *oxidezap*.
//! Velo 1.0 kept the oxidezap name everywhere a user could not see it: the data
//! folder, the downloads folder, the registry, the sign-in entry and the
//! notification identity. 1.1 used Velo's own names, and Wasp now uses its own.
//! This module is the bridge, and the one place in the tree that still has to
//! spell the old ones.
//!
//! Every Wasp program calls [`migrate`] first thing, before it opens a file,
//! so whichever starts first does the move and the others find it done. Each
//! step only renames into a name that does not exist yet - nothing is ever
//! overwritten or merged - and is idempotent, so a second call, a second
//! program, or a crash halfway through all leave a consistent state. A folder
//! that cannot be moved (another program holding a file in it) is left where
//! it is and logged; the installer stops every Wasp process before it runs
//! the new version, so in practice nothing holds them.

/// Move anything still under an old name to Wasp's. Cheap when there is
/// nothing to do: a directory listing per base folder, a registry probe.
///
/// Returns what it did, one line per move, for the caller to log: it runs
/// before logging is set up, because a logger that opens its file under the
/// new data folder would create that folder and so block the move into it.
#[must_use = "log what the migration did once logging is up"]
pub fn migrate() -> Vec<String> {
    imp::migrate();
    std::mem::take(&mut *notes())
}

fn notes() -> std::sync::MutexGuard<'static, Vec<String>> {
    static NOTES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    NOTES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg_attr(target_family = "wasm", allow(dead_code))]
fn note(line: String) {
    notes().push(line);
}

/// The data folder's new name, and the prefix per-account folders share.
pub const NEW: &str = "wasp";
/// Names this product used before Wasp, newest first.
const OLD_NAMES: [&str; 2] = ["velo", "oxidezap"];

/// What an entry in a base folder is called after the move, if it is one of
/// ours: the folder itself, or a per-account `<name>-<account>` beside it.
fn renamed(name: &str) -> Option<String> {
    if name == NEW || name.starts_with("wasp-") {
        return None;
    }
    for old in OLD_NAMES {
        if name == old {
            return Some(NEW.to_owned());
        }
        if let Some(account) = name
            .strip_prefix(old)
            .and_then(|rest| rest.strip_prefix('-'))
            && !account.is_empty()
        {
            return Some(format!("{NEW}-{account}"));
        }
    }
    None
}

#[cfg(target_family = "wasm")]
mod imp {
    pub(super) fn migrate() {}
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::path::{Path, PathBuf};

    pub(super) fn migrate() {
        for base in bases() {
            rename_children(&base);
        }
        #[cfg(windows)]
        {
            downloads();
            windows::registry();
        }
    }

    /// The folders Velo keeps state under, on each platform.
    fn bases() -> Vec<PathBuf> {
        let mut bases = Vec::new();
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        #[cfg(windows)]
        {
            bases.extend(var("LOCALAPPDATA"));
            bases.extend(var("APPDATA"));
        }
        #[cfg(not(windows))]
        {
            let home = var("HOME");
            let xdg = |name: &str, fallback: &str| {
                var(name).or_else(|| home.as_ref().map(|home| home.join(fallback)))
            };
            bases.extend(xdg("XDG_DATA_HOME", ".local/share"));
            bases.extend(xdg("XDG_CONFIG_HOME", ".config"));
            bases.extend(xdg("XDG_STATE_HOME", ".local/state"));
            bases.extend(xdg("XDG_CACHE_HOME", ".cache"));
            #[cfg(target_os = "macos")]
            if let Some(home) = &home {
                bases.push(home.join("Library/Application Support"));
                bases.push(home.join("Library/Caches"));
            }
        }
        bases
    }

    /// Rename every old-named entry directly under `base`.
    pub(super) fn rename_children(base: &Path) {
        let Ok(entries) = std::fs::read_dir(base) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Some(new) = super::renamed(&name) {
                move_into_place(&entry.path(), &base.join(new));
            }
        }
    }

    /// Rename `from` to `to` unless `to` already exists. A file held open by
    /// a program that is still shutting down is retried briefly.
    pub(super) fn move_into_place(from: &Path, to: &Path) {
        if to.exists() {
            super::note(format!(
                "{} was not moved: {} already exists",
                from.display(),
                to.display()
            ));
            return;
        }
        let mut last = None;
        for _ in 0..10 {
            match std::fs::rename(from, to) {
                Ok(()) => {
                    super::note(format!("moved {} to {}", from.display(), to.display()));
                    return;
                }
                // Another Velo program starting at the same moment got there
                // first.
                Err(_) if !from.exists() && to.exists() => return,
                Err(error) => {
                    last = Some(error);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
        if let Some(error) = last {
            super::note(format!("{} could not be moved: {error}", from.display()));
        }
    }

    /// Saved files: `Downloads\OxideZap` and `Downloads\Velo` become
    /// `Downloads\Wasp`.
    #[cfg(windows)]
    fn downloads() {
        let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) else {
            return;
        };
        let downloads = PathBuf::from(profile).join("Downloads");
        for old in ["OxideZap", "Velo"] {
            let old = downloads.join(old);
            if old.is_dir() {
                move_into_place(&old, &downloads.join("Wasp"));
            }
        }
    }

    #[cfg(windows)]
    mod windows {
        use windows_registry::CURRENT_USER;

        const OLD_PREFS: [&str; 2] = [r"Software\OxideZap", r"Software\Velo"];
        const NEW_PREFS: &str = r"Software\Wasp";
        const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
        const OLD_RUN: [&str; 2] = ["OxideZap", "Velo"];
        const NEW_RUN: &str = "Wasp";
        const OLD_APP_IDS: [&str; 2] = [
            r"Software\Classes\AppUserModelId\org.oxidezap.client.local",
            r"Software\Classes\AppUserModelId\io.github.RuchinAudichya.Velo",
        ];

        pub(super) fn registry() {
            preferences();
            sign_in_entry();
            // The old notification identities: Windows keeps showing their
            // names in Settings until the keys are gone.
            for app_id in OLD_APP_IDS {
                let _ = CURRENT_USER.remove_tree(app_id);
            }
        }

        /// Copy each saved preference the new key does not have yet, then
        /// drop the old keys.
        fn preferences() {
            let Ok(new) = CURRENT_USER.create(NEW_PREFS) else {
                return;
            };
            for old in OLD_PREFS {
                let Ok(old) = CURRENT_USER.open(old) else {
                    continue;
                };
                if let Ok(values) = old.values() {
                    for (name, value) in values {
                        if new.get_value(&name).is_err() {
                            let _ = new.set_value(&name, &value);
                        }
                    }
                }
                drop(old);
            }
            for old in OLD_PREFS {
                let _ = CURRENT_USER.remove_tree(old);
            }
        }

        /// "Start Velo when you sign in" was stored under an old name and
        /// pointed at the old program. Carry the choice over to the new name
        /// and program; the installer writes the same, this covers a portable
        /// copy that never ran it.
        fn sign_in_entry() {
            let Ok(run) = CURRENT_USER.create(RUN) else {
                return;
            };
            let mut wanted = false;
            for old in OLD_RUN {
                if run.get_string(old).is_ok() {
                    wanted = true;
                    let _ = run.remove_value(old);
                }
            }
            if !wanted || run.get_string(NEW_RUN).is_ok() {
                return;
            }
            let service = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("waspd.exe")))
                .filter(|path| path.is_file());
            if let Some(service) = service {
                let _ = run.set_string(NEW_RUN, format!("\"{}\"", service.display()));
            }
        }
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn the_data_folder_and_account_folders_take_the_new_name() {
        assert_eq!(renamed("oxidezap").as_deref(), Some("wasp"));
        assert_eq!(renamed("velo").as_deref(), Some("wasp"));
        assert_eq!(renamed("oxidezap-work").as_deref(), Some("wasp-work"));
        assert_eq!(renamed("velo-work").as_deref(), Some("wasp-work"));
        assert_eq!(renamed("oxidezap-"), None);
        assert_eq!(renamed("oxidezapper"), None);
        assert_eq!(renamed("wasp"), None);
        assert_eq!(renamed("wasp-work"), None);
        assert_eq!(renamed("Something else"), None);
    }

    #[test]
    fn folders_move_once_and_never_over_an_existing_one() {
        let base = std::env::temp_dir().join(format!("wasp-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("oxidezap")).unwrap();
        std::fs::write(base.join("oxidezap").join("whatsapp.db"), b"store").unwrap();
        std::fs::create_dir_all(base.join("oxidezap-work")).unwrap();
        std::fs::create_dir_all(base.join("velo-work")).unwrap();
        std::fs::create_dir_all(base.join("velo-home")).unwrap();
        std::fs::create_dir_all(base.join("wasp-home")).unwrap();
        std::fs::write(base.join("wasp-home").join("keep"), b"new").unwrap();

        imp::rename_children(&base);

        // Moved, contents intact.
        assert_eq!(
            std::fs::read(base.join("wasp").join("whatsapp.db")).unwrap(),
            b"store"
        );
        assert!(!base.join("oxidezap").exists());
        assert!(base.join("wasp-work").is_dir());
        assert!(base.join("wasp-home").join("keep").is_file());
        // A new-named folder already there is never replaced: the old one is
        // left for a person to look at rather than merged or lost.
        assert!(base.join("velo-home").is_dir());

        // Running again changes nothing.
        imp::rename_children(&base);
        assert!(base.join("wasp").join("whatsapp.db").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }
}
