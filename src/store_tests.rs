// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_build_name_is_digits_with_no_leading_zero_and_not_zero() {
    assert_eq!(
        plain_build_name("1000001000000000070"),
        Some(1_000_001_000_000_000_070)
    );
    assert_eq!(plain_build_name("8256"), Some(8256));
    assert_eq!(plain_build_name("8256.noindex"), Some(8256));
    for bad in ["", "0", "0123", "12a", "current", "-1", "1.ready", " 12"] {
        assert_eq!(plain_build_name(bad), None, "{bad:?}");
    }
}

#[test]
fn a_vendor_build_id_decodes_to_its_version_in_both_tiers() {
    // The four ids measured in aterm's store (packaging report, 2026-10-24).
    assert_eq!(
        decode_vendor_build(1_000_002_000_001_000_310).as_deref(),
        Some("2.1.310")
    );
    assert_eq!(
        decode_vendor_build(1_000_001_000_004_000_007).as_deref(),
        Some("1.4.7")
    );
    assert_eq!(
        decode_vendor_build(2_001_004_013_006_996_002).as_deref(),
        Some("1.4.13-R6996.1")
    );
    assert_eq!(
        decode_vendor_build(2_001_004_013_006_996_000).as_deref(),
        Some("1.4.13-R6996")
    );
    // Grok Build 1.0.70, the stable head measured 2026-10-24.
    assert_eq!(
        decode_vendor_build(1_000_001_000_000_000_070).as_deref(),
        Some("1.0.70")
    );
    // ALab build numbers and ids past the tiers name no vendor version.
    assert_eq!(decode_vendor_build(2_026_102_001), None);
    assert_eq!(decode_vendor_build(3_000_000_000_000_000_000), None);
}

#[test]
fn the_vendor_record_gives_its_version_line_and_nothing_else() {
    let record = "schema = 1\nprogram = \"grok\"\nversion = \"1.0.70\"\nvendor = \"xAI\"\n";
    assert_eq!(vendor_version(record).as_deref(), Some("1.0.70"));
    assert_eq!(
        vendor_version("version=\"2.1.310\"").as_deref(),
        Some("2.1.310")
    );
    assert_eq!(vendor_version("schema = 1\n"), None);
    assert_eq!(vendor_version("version = \"1.0 ; rm -rf\"\n"), None);
    assert_eq!(vendor_version("version = 1.0.70\n"), None);
    assert_eq!(vendor_version("versions = \"1\"\n"), None);
}

#[test]
fn a_record_names_its_build_only_with_the_version_the_id_encodes() {
    let id = 1_000_001_000_000_000_070;
    assert!(record_names_build("1.0.70", id));
    for other in [
        "1.0.71",
        "1.0.070",
        "01.0.70",
        "1.0.70-R1",
        "1.0",
        "",
        "build 1",
    ] {
        assert!(!record_names_build(other, id), "{other:?}");
    }
    let revision = 2_001_004_013_006_996_002;
    assert!(record_names_build("1.4.13-R6996.1", revision));
    assert!(!record_names_build("1.4.13-R6996", revision));
    // An ALab build number names no vendor version, so no record can name it.
    assert!(!record_names_build("0.0.2026102001", 2_026_102_001));
}

#[test]
fn a_shim_env_sidecar_is_admitted_whole_or_not_at_all() {
    assert_eq!(
        admit_shim_env("GROK_DISABLE_AUTOUPDATER=1\n"),
        vec![("GROK_DISABLE_AUTOUPDATER".to_string(), "1".to_string())]
    );
    assert_eq!(
        admit_shim_env(
            "# comment\n\nDISABLE_AUTOUPDATER=1\nCLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1\n"
        )
        .len(),
        2
    );
    // One bad line refuses the whole sidecar (atpkg's fail-closed `from_lines`).
    for bad in [
        "GROK_DISABLE_AUTOUPDATER=1\nPATH=/evil\n",
        "DYLD_INSERT_LIBRARIES=/x.dylib\n",
        "lower=1\n",
        "1ABC=1\n",
        "A=\n",
        "A= 1\n",
        "A=\"q\"\n",
        "A=50%\n",
        "A=1\nA=2\n",
        "NOEQUALS\n",
        "A=1\nB=1\nC=1\nD=1\nE=1\nF=1\nG=1\nH=1\nI=1\n",
    ] {
        assert_eq!(admit_shim_env(bad), Vec::new(), "{bad:?}");
    }
}

#[test]
fn the_self_update_switch_is_always_off_whatever_the_sidecar_says() {
    let off = (AUTOUPDATER_OFF.0.to_string(), "1".to_string());
    assert_eq!(with_autoupdater_off(Vec::new()), vec![off.clone()]);
    for value in ["0", "false", "1"] {
        let env = with_autoupdater_off(admit_shim_env(&format!(
            "OTHER=x\nGROK_DISABLE_AUTOUPDATER={value}\n"
        )));
        assert_eq!(
            env,
            vec![("OTHER".to_string(), "x".to_string()), off.clone()]
        );
    }
}

#[test]
fn the_config_prefix_is_read_from_the_packages_table_only() {
    assert_eq!(
        config_prefix("[packages]\nprefix = \"/opt/aterm/pkg\"\n").as_deref(),
        Some("/opt/aterm/pkg")
    );
    assert_eq!(
        config_prefix("[packages]\nprefix = '~/pkg' # mine\n").as_deref(),
        Some("~/pkg")
    );
    assert_eq!(
        config_prefix("packages.prefix = \"/p\"\n[other]\nprefix = \"/q\"\n").as_deref(),
        Some("/p")
    );
    assert_eq!(config_prefix("[harness]\nprefix = \"/q\"\n"), None);
    assert_eq!(config_prefix("[packages.links]\nprefix = \"/q\"\n"), None);
    assert_eq!(config_prefix("[packages]\nprefix = \"/a\\\\b\"\n"), None);
    assert_eq!(config_prefix("[packages]\nprefix = \"/a\" junk\n"), None);
    assert_eq!(config_prefix("[packages]\nprefix = \"\"\"#\"\"\"\n"), None);
    assert_eq!(
        config_prefix("note = \"\"\"\n[packages]\nprefix = \"/q\"\n\"\"\"\n"),
        None
    );
    assert_eq!(config_prefix("[[packages]]\nprefix = \"/q\"\n"), None);
    assert_eq!(config_prefix(""), None);
}

fn env_of(pairs: &[(&str, &str)]) -> Env {
    Env::from_pairs(pairs.iter().copied())
}

#[test]
fn the_prefix_override_must_be_absolute() {
    let get = env_of(&[(PREFIX_ENV, "/somewhere/pkg"), ("HOME", "/h")]);
    assert_eq!(
        resolve_prefix(&get).unwrap().prefix,
        PathBuf::from("/somewhere/pkg")
    );
    for bad in ["relative/pkg", "/a/../b"] {
        let get = env_of(&[(PREFIX_ENV, bad), ("HOME", "/h")]);
        assert!(resolve_prefix(&get).is_err(), "{bad}");
    }
}

#[test]
fn with_no_override_and_no_config_the_prefix_is_atpkgs_default() {
    let get = env_of(&[
        ("HOME", "/nonexistent-home-for-grok-alab-tests"),
        ("XDG_CONFIG_HOME", "/nonexistent-config-for-grok-alab-tests"),
    ]);
    let want = if cfg!(target_os = "macos") {
        "/nonexistent-home-for-grok-alab-tests/Library/Application Support/aterm/pkg"
    } else {
        "/nonexistent-home-for-grok-alab-tests/.local/share/aterm/pkg"
    };
    assert_eq!(resolve_prefix(&get).unwrap().prefix, PathBuf::from(want));
}

/// The account's home as the system's own tool reads it from the password database, not
/// through this crate: `id -P` on macOS (a master.passwd(5) line), `getent passwd <uid>`
/// elsewhere (a passwd(5) line). The home is the second-to-last field of both.
fn home_in_the_password_database() -> PathBuf {
    use std::process::Command;
    let out = if cfg!(target_os = "macos") {
        Command::new("/usr/bin/id").arg("-P").output().unwrap()
    } else {
        let uid = Command::new("id").arg("-u").output().unwrap();
        assert!(uid.status.success(), "id -u: {uid:?}");
        let uid = String::from_utf8(uid.stdout).unwrap();
        Command::new("getent")
            .args(["passwd", uid.trim()])
            .output()
            .unwrap()
    };
    assert!(
        out.status.success(),
        "the password database lookup: {out:?}"
    );
    let line = String::from_utf8(out.stdout).unwrap();
    let home = line.trim_end().rsplit(':').nth(1).unwrap_or_default();
    let home = PathBuf::from(home);
    assert!(home.is_absolute(), "no absolute home in {line:?}");
    home
}

/// An environment with `home` as `HOME` (`None`: unset) and a config dir that does not exist,
/// so the prefix is atpkg's default under whichever home was taken.
fn env_with_home(home: Option<&str>) -> Env {
    let mut pairs = vec![("XDG_CONFIG_HOME", "/nonexistent-config-for-grok-alab-tests")];
    if let Some(h) = home {
        pairs.push(("HOME", h));
    }
    env_of(&pairs)
}

#[test]
fn the_account_home_is_the_password_databases() {
    assert_eq!(account_home(), Some(home_in_the_password_database()));
}

#[test]
fn a_home_that_is_not_absolute_is_the_password_databases() {
    // Before 59797c1 a relative `HOME` fell back to the password database; 59797c1 found no
    // home for one and `run` exited 2 before reading its arguments.
    let want = default_prefix(&home_in_the_password_database());
    for home in ["relhome", "./relhome", "relhome/../x", "~", "~/x"] {
        assert_eq!(
            resolve_prefix(&env_with_home(Some(home))).unwrap().prefix,
            want,
            "HOME={home:?}"
        );
    }
}

#[test]
fn an_unset_or_empty_home_is_the_password_databases() {
    let want = default_prefix(&home_in_the_password_database());
    for env in [env_with_home(None), env_with_home(Some(""))] {
        assert_eq!(resolve_prefix(&env).unwrap().prefix, want, "{env:?}");
    }
}

#[test]
fn an_absolute_home_is_used_as_it_is() {
    // A home the password database does not name, so a fallback to it would show.
    let home = "/nonexistent-home-for-grok-alab-tests";
    let got = resolve_prefix(&env_with_home(Some(home))).unwrap().prefix;
    assert_eq!(got, default_prefix(Path::new(home)));
    assert_ne!(got, default_prefix(&home_in_the_password_database()));
}

#[test]
fn the_layout_names_the_store_paths_atpkg_writes() {
    let l = Layout {
        prefix: PathBuf::from("/p"),
    };
    assert_eq!(l.program_dir(), PathBuf::from("/p/store/grok"));
    assert_eq!(l.current_link(), PathBuf::from("/p/store/grok/current"));
}

#[test]
fn ready_markers_require_ok_and_match_the_running_platform() {
    assert!(ready_text_accepts(
        "ok\nplatform=aarch64-macos\ncontents=bin:grok\n",
        "aarch64-macos"
    ));
    assert!(ready_text_accepts("ok\n", "aarch64-macos"));
    assert!(!ready_text_accepts(
        "ok\nplatform=x86_64-linux\n",
        "aarch64-macos"
    ));
    assert!(!ready_text_accepts(
        "pending\nplatform=aarch64-macos\n",
        "aarch64-macos"
    ));
    assert!(!ready_text_accepts("", "aarch64-macos"));
}

#[test]
fn read_bounded_refuses_oversized_and_non_regular_files() {
    let dir = crate::scratch::Scratch::new("bounded");
    let small = dir.join("small.txt");
    std::fs::write(&small, "ok\n").unwrap();
    assert_eq!(read_bounded(&small, 16).as_deref(), Some("ok\n"));
    assert_eq!(read_bounded(&small, 2), None);
    assert_eq!(read_sidecar(&small, 16).as_deref(), Some("ok\n"));
    let link = dir.join("link.txt");
    std::os::unix::fs::symlink(&small, &link).unwrap();
    assert_eq!(read_sidecar(&link, 16), None, "symlink sidecar refused");
    assert_eq!(read_bounded(&link, 16).as_deref(), Some("ok\n"));
    let fifo = dir.join("fifo.txt");
    let c_fifo =
        std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(fifo.as_os_str())).unwrap();
    // SAFETY: `c_fifo` is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    assert_eq!(read_bounded(&fifo, 16), None);
    assert_eq!(read_sidecar(&fifo, 16), None);
}
