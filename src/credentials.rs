// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHETHER GROK WOULD START SIGNED IN, read from its environment and files alone, before the
//! harness starts a grok nobody typed.
//!
//! Grok Build 1.0.70, started signed out, draws xAI's device sign-in instead of a composer and
//! opens the sign-in page in the desk's default browser through macOS's LaunchServices, whatever
//! its `PATH` (aterm `docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md` §5(g); the four stray
//! "Device Sign-in | xAI" tabs of 2026-10-25 came from exactly that). A launch the person typed
//! is theirs. A RELAUNCH is the harness's own (after a crash, or to finish a move), so it never
//! starts a grok that this check says is signed out: the session ends there instead, and no page
//! opens that nobody asked for.
//!
//! The reading is aterm's own row for Grok Build (`crates/aterm-agent/src/ai/connector.rs`: the
//! credential variables `GROK_AUTH` and `XAI_API_KEY`, the login file `GROK_AUTH_PATH` names, else
//! `$GROK_HOME/auth.json`, `~/.grok` by default). It reads presence, never contents: a file that
//! is there but stale passes, except one aterm saw grok refuse. aterm writes a refusal marker
//! beside it (`auth.json.refused`, aterm's `harness::grok_signin::mark_refused`) when a grok
//! started after that login still drew its sign-in screen, and a login no newer than its marker
//! reads signed out ([`REFUSED_SUFFIX`]); a new login replaces it. An `api_key` in
//! `~/.grok/config.toml` is not read, so such a setup reads signed out and is not relaunched
//! (fail closed: a session ends rather than a page opens).

use std::path::{Path, PathBuf};

use crate::env::Env;

/// Variables whose non-empty value signs a launch in ahead of any login file.
pub const CREDENTIAL_ENV: [&str; 2] = ["GROK_AUTH", "XAI_API_KEY"];

/// The variable naming the login file Grok then reads in place of its home's.
pub const LOGIN_FILE_ENV: &str = "GROK_AUTH_PATH";

/// The variable naming Grok's home (default `~/.grok`).
pub const HOME_ENV: &str = "GROK_HOME";

/// The login file in Grok's home.
pub const LOGIN_FILE: &str = "auth.json";

/// The suffix of a login file's refusal marker (`auth.json.refused`): aterm writes it when a
/// grok started after that login still drew its sign-in screen (aterm's
/// `harness::grok_signin::mark_refused`), and a login at most as new as its marker is signed
/// out. aterm's own reader and atpkg's twin read the marker the same way.
pub const REFUSED_SUFFIX: &str = ".refused";

/// Whether `path` is a non-empty regular file (a symlink to one counts, as it does for Grok)
/// that grok has not refused: no refusal marker ([`REFUSED_SUFFIX`]) at least as new as it.
fn non_empty_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    let mut marker = path.as_os_str().to_os_string();
    marker.push(REFUSED_SUFFIX);
    match std::fs::symlink_metadata(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Ok(m) if m.is_file() => match (meta.modified(), m.modified()) {
            (Ok(written), Ok(marked)) => written > marked,
            _ => false,
        },
        _ => false,
    }
}

/// Whether grok, started with environment `env`, would start signed in: a non-empty
/// [`CREDENTIAL_ENV`] value; else, when [`LOGIN_FILE_ENV`] is set, that file alone (non-empty);
/// else `${GROK_HOME:-$HOME/.grok}/auth.json` (non-empty). A login file no newer than its
/// refusal marker is signed out ([`REFUSED_SUFFIX`]). No home to read is signed out.
#[must_use]
pub fn signed_in(env: &Env) -> bool {
    if CREDENTIAL_ENV
        .iter()
        .any(|n| env.get(n).is_some_and(|v| !v.is_empty()))
    {
        return true;
    }
    if let Some(file) = env.get(LOGIN_FILE_ENV).filter(|v| !v.is_empty()) {
        return non_empty_file(Path::new(&file));
    }
    let home = match env.get(HOME_ENV).filter(|v| !v.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => match env.get("HOME").filter(|v| !v.is_empty()) {
            Some(h) => PathBuf::from(h).join(".grok"),
            None => return false,
        },
    };
    non_empty_file(&home.join(LOGIN_FILE))
}

/// The environment a child of the harness runs with: the build's shim environment over the
/// harness's own `env` (what [`crate::supervise`] gives the child).
#[must_use]
pub fn child_env(shim: &[(String, String)], env: &Env) -> Env {
    env.over(shim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::ffi::OsString;

    /// A scratch home, removed when the test ends, passed or failed.
    fn scratch(name: &str) -> Scratch {
        Scratch::new(&format!("cred-{name}"))
    }

    fn env(pairs: Vec<(&'static str, OsString)>) -> Env {
        Env::from_pairs(pairs)
    }

    #[test]
    fn a_credential_variable_or_a_non_empty_login_file_signs_grok_in() {
        let home = scratch("home");
        let h = || ("HOME", home.as_os_str().to_owned());
        // Nothing at all: signed out.
        assert!(!signed_in(&env(vec![h()])));
        assert!(!signed_in(&env(vec![])), "no home to read is signed out");
        // An empty auth.json is no sign-in.
        std::fs::create_dir_all(home.join(".grok")).unwrap();
        std::fs::write(home.join(".grok").join(LOGIN_FILE), b"").unwrap();
        assert!(!signed_in(&env(vec![h()])));
        std::fs::write(home.join(".grok").join(LOGIN_FILE), b"{}").unwrap();
        assert!(signed_in(&env(vec![h()])));
        // GROK_AUTH_PATH names the only file read, present or not.
        assert!(!signed_in(&env(vec![
            h(),
            (LOGIN_FILE_ENV, home.join("missing.json").into_os_string())
        ])));
        // GROK_HOME moves the home.
        let other = scratch("other");
        assert!(!signed_in(&env(vec![
            h(),
            (HOME_ENV, other.as_os_str().to_owned())
        ])));
        std::fs::write(other.join(LOGIN_FILE), b"{}").unwrap();
        assert!(signed_in(&env(vec![(
            HOME_ENV,
            other.as_os_str().to_owned()
        )])));
        // A credential variable signs in whatever the files say; an empty one does not.
        let bare = scratch("bare");
        let b = || ("HOME", bare.as_os_str().to_owned());
        assert!(signed_in(&env(vec![b(), ("XAI_API_KEY", "k".into())])));
        assert!(signed_in(&env(vec![b(), ("GROK_AUTH", "t".into())])));
        assert!(!signed_in(&env(vec![b(), ("XAI_API_KEY", "".into())])));
    }

    #[test]
    fn a_login_at_most_as_new_as_its_refusal_marker_is_signed_out() {
        let home = scratch("refused");
        std::fs::create_dir_all(home.join(".grok")).unwrap();
        let auth = home.join(".grok").join(LOGIN_FILE);
        std::fs::write(&auth, b"{}").unwrap();
        let get = env(vec![("HOME", home.as_os_str().to_owned())]);
        assert!(signed_in(&get), "CONTROL: no marker");
        let marker = home
            .join(".grok")
            .join(format!("{LOGIN_FILE}{REFUSED_SUFFIX}"));
        std::fs::write(&marker, b"").unwrap();
        let marked = std::fs::metadata(&marker).unwrap().modified().unwrap();
        let set = |at: std::time::SystemTime| {
            std::fs::File::options()
                .write(true)
                .open(&auth)
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        set(marked);
        assert!(!signed_in(&get), "as new as its marker");
        set(marked - std::time::Duration::from_secs(5));
        assert!(!signed_in(&get), "older than its marker");
        set(marked + std::time::Duration::from_secs(5));
        assert!(signed_in(&get), "a newer login");
        // The named login file is read the same way.
        set(marked);
        assert!(!signed_in(&env(vec![(
            LOGIN_FILE_ENV,
            auth.clone().into_os_string()
        )])));
        // A dangling symlink at the refusal marker still fails closed (signed out).
        std::fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink(home.join("nonexistent"), &marker).unwrap();
        assert!(!signed_in(&get), "a symlinked refusal marker fails closed");
    }

    #[test]
    fn the_shim_environment_wins_over_the_harness_own() {
        let shim = vec![("GROK_HOME".to_string(), "/from/shim".to_string())];
        let own = Env::from_pairs([("GROK_HOME", "/mine"), ("HOME", "/mine")]);
        let child = child_env(&shim, &own);
        assert_eq!(child.get("GROK_HOME"), Some(OsString::from("/from/shim")));
        assert_eq!(child.get("HOME"), Some(OsString::from("/mine")));
        assert_eq!(child.get("XAI_API_KEY"), None);
    }
}
