use std::path::{Path, PathBuf};

use super::*;

fn archive_probe() -> Probe {
    Probe::default()
}

fn channel_of(exe: &str) -> Channel {
    Channel::of(Path::new(exe), &archive_probe())
}

#[test]
fn a_cellar_path_is_homebrew_on_both_prefixes() {
    for exe in [
        "/opt/homebrew/Cellar/shep/0.24.0/bin/shep",
        "/usr/local/Cellar/shep/0.24.0/bin/shep",
        "/home/linuxbrew/.linuxbrew/Cellar/shep/0.24.0/bin/shep",
    ] {
        assert_eq!(channel_of(exe), Channel::Homebrew, "{exe}");
    }
}

#[test]
fn a_binary_in_cargo_homes_bin_is_cargo_and_cargo_home_moves_it() {
    let default = Probe {
        cargo_home: Some(PathBuf::from("/home/ann/.cargo")),
        ..Probe::default()
    };
    let moved = Probe {
        cargo_home: Some(PathBuf::from("/opt/cargo")),
        ..Probe::default()
    };

    let in_default = Path::new("/home/ann/.cargo/bin/shep");
    let in_moved = Path::new("/opt/cargo/bin/shep");
    assert_eq!(Channel::of(in_default, &default), Channel::Cargo);
    assert_eq!(Channel::of(in_moved, &moved), Channel::Cargo);
    assert_eq!(Channel::of(in_default, &moved), Channel::Archive);
    assert_eq!(Channel::of(in_moved, &default), Channel::Archive);
    assert_eq!(
        Channel::of(Path::new("/home/ann/.cargo/registry/shep"), &default),
        Channel::Archive,
        "only the bin directory itself counts"
    );
}

#[test]
fn a_scoop_apps_path_is_scoop_whatever_the_case_or_separator() {
    for exe in [
        r"C:\Users\Ann\scoop\apps\shep\current\shep.exe",
        r"c:\users\ann\SCOOP\APPS\SHEP\0.24.0\shep.exe",
        "C:/Users/Ann/scoop/apps/shep/current/shep.exe",
    ] {
        assert_eq!(channel_of(exe), Channel::Scoop, "{exe}");
    }
}

#[test]
fn a_scoop_root_from_the_environment_is_scoop() {
    let probe = Probe {
        scoop_roots: vec![PathBuf::from(r"D:\Tools\Scoop"), PathBuf::from("E:/global")],
        ..Probe::default()
    };

    for exe in [
        r"D:\Tools\Scoop\apps\shep\current\shep.exe",
        r"E:\Global\apps\shep\0.24.0\shep.exe",
    ] {
        assert_eq!(Channel::of(Path::new(exe), &probe), Channel::Scoop, "{exe}");
    }
    assert_eq!(
        Channel::of(Path::new(r"F:\Tools\apps\shep\shep.exe"), &probe),
        Channel::Archive,
        "apps/shep counts only under a named root"
    );
}

#[test]
fn a_winget_packages_path_is_winget() {
    for exe in [
        r"C:\Users\Ann\AppData\Local\Microsoft\WinGet\Packages\shep-pm.shep_Microsoft.Winget.Source_8wekyb3d8bbwe\shep.exe",
        r"c:\program files\winget\packages\shep-pm.shep_x\shep.exe",
    ] {
        assert_eq!(channel_of(exe), Channel::Winget, "{exe}");
    }
    assert_eq!(
        channel_of(r"C:\Users\Ann\Packages\WinGet\shep.exe"),
        Channel::Archive,
        "Packages has to come after WinGet"
    );
}

#[test]
fn a_chocolatey_lib_path_is_chocolatey() {
    let probe = Probe {
        chocolatey_root: Some(PathBuf::from(r"D:\Choco")),
        ..Probe::default()
    };

    assert_eq!(
        channel_of(r"C:\ProgramData\chocolatey\lib\shep\tools\shep.exe"),
        Channel::Chocolatey
    );
    assert_eq!(
        Channel::of(Path::new(r"D:\choco\lib\shep\tools\shep.exe"), &probe),
        Channel::Chocolatey
    );
    assert_eq!(
        Channel::of(Path::new(r"C:\tools\lib\shep\shep.exe"), &probe),
        Channel::Archive
    );
}

fn dpkg_info_with(list: &str, body: &str) -> (tempfile::TempDir, Probe) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(list), body).unwrap();
    let probe = Probe {
        dpkg_info: dir.path().to_path_buf(),
        ..Probe::default()
    };
    (dir, probe)
}

#[test]
fn usr_bin_shep_named_by_dpkg_is_deb_and_unnamed_is_archive() {
    let exe = Path::new("/usr/bin/shep");
    let (_named, named) = dpkg_info_with("shep.list", "/.\n/usr\n/usr/bin\n/usr/bin/shep\n");
    let (_other, other) = dpkg_info_with("shep.list", "/usr/bin/shepherd\n");
    let (_elsewhere, elsewhere) = dpkg_info_with("coreutils.list", "/usr/bin/shep\n");

    assert_eq!(Channel::of(exe, &named), Channel::Deb);
    assert_eq!(Channel::of(exe, &other), Channel::Archive);
    assert_eq!(
        Channel::of(exe, &elsewhere),
        Channel::Archive,
        "only a list of the shep package counts"
    );
    assert_eq!(
        Channel::of(exe, &Probe::default()),
        Channel::Archive,
        "no dpkg directory is no package"
    );
    assert_eq!(
        Channel::of(Path::new("/usr/local/bin/shep"), &named),
        Channel::Archive,
        "dpkg names /usr/bin/shep, not this binary"
    );
}

#[test]
fn dpkg_lists_for_shep_and_for_shep_with_an_arch_are_both_read() {
    let exe = Path::new("/usr/bin/shep");
    let (_arch, arch) = dpkg_info_with("shep:amd64.list", "/usr/bin/shep\n");
    let (_other, other) = dpkg_info_with("shepherd:amd64.list", "/usr/bin/shep\n");

    assert_eq!(Channel::of(exe, &arch), Channel::Deb);
    assert_eq!(Channel::of(exe, &other), Channel::Archive);
}

#[test]
fn a_verbatim_windows_prefix_does_not_hide_the_channel() {
    assert_eq!(
        channel_of(r"\\?\C:\Users\Ann\scoop\apps\shep\current\shep.exe"),
        Channel::Scoop
    );
    assert_eq!(
        channel_of(r"\\?\C:\ProgramData\chocolatey\lib\shep\tools\shep.exe"),
        Channel::Chocolatey
    );
    let probe = Probe {
        cargo_home: Some(PathBuf::from(r"C:\Users\Ann\.cargo")),
        ..Probe::default()
    };
    assert_eq!(
        Channel::of(Path::new(r"\\?\C:\Users\Ann\.cargo\bin\shep.exe"), &probe),
        Channel::Cargo
    );
}

#[test]
fn a_near_miss_is_not_a_match() {
    for exe in [
        "/opt/homebrew/Cellar/shep-other/1.0/bin/shep",
        "/opt/homebrew/Cellar/shepherd/1.0/bin/shep",
        r"C:\Users\Ann\scoop\apps\shepherd\current\shep.exe",
        r"C:\Users\Ann\scoop\apps\shep-other\current\shep.exe",
        r"C:\ProgramData\chocolatey\lib\shepherd\tools\shep.exe",
        r"C:\Users\Ann\AppData\Local\Microsoft\WinGet\Links\shep.exe",
        r"C:\Users\Ann\AppData\Local\Microsoft\WinGetPackages\shep.exe",
    ] {
        assert_eq!(channel_of(exe), Channel::Archive, "{exe}");
    }
}

#[test]
fn each_manager_names_its_command_and_deb_and_archive_name_none() {
    assert_eq!(
        Channel::Homebrew.command().as_deref(),
        Some("brew upgrade shep")
    );
    assert_eq!(
        Channel::Cargo.command().as_deref(),
        Some("cargo install --locked shep")
    );
    assert_eq!(
        Channel::Scoop.command().as_deref(),
        Some("scoop update shep")
    );
    assert_eq!(
        Channel::Winget.command().as_deref(),
        Some("winget upgrade shep-pm.shep")
    );
    assert_eq!(
        Channel::Chocolatey.command().as_deref(),
        Some("choco upgrade shep")
    );
    assert_eq!(Channel::Deb.command().as_deref(), None);
    assert_eq!(Channel::Archive.command().as_deref(), None);
}
