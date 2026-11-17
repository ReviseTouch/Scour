//! The Windows service mode, step 2 of docs/WINDOWS-SERVICE.md: moving an
//! installation into Program Files under the `Scour` service, and back. The
//! window asks; a copy of itself started with `runas` does the part that needs
//! an administrator, and nothing else. Settings stay in the person's folder.

/// Whether this platform has the choice at all.
pub const OFFERED: bool = cfg!(windows);

/// Whether the index is kept by the `Scour` Windows service now.
pub fn active() -> bool {
    scour_launch::windows_service_registered()
}

#[cfg(windows)]
pub use imp::{elevated_off, elevated_on, switch};

#[cfg(not(windows))]
pub fn switch(_to_service: bool, _config: &scour_config::Config) -> Result<(), String> {
    Err("only on Windows".into())
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SERVICE: &str = "Scour";

    fn quiet(program: &str, args: &[&str]) -> Result<std::process::Output, String> {
        Command::new(program)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("{program}: {e}"))
    }

    fn program_files() -> Result<PathBuf, String> {
        std::env::var_os("ProgramFiles")
            .map(|d| PathBuf::from(d).join("Scour"))
            .ok_or_else(|| "ProgramFiles is not set".into())
    }

    fn program_data() -> Result<PathBuf, String> {
        std::env::var_os("ProgramData")
            .map(|d| PathBuf::from(d).join("Scour"))
            .ok_or_else(|| "ProgramData is not set".into())
    }

    fn here() -> Result<PathBuf, String> {
        std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "no folder for this program".into())
    }

    /// The account's SID, as `whoami` prints it: the one name the pipe's
    /// access list can hold whatever the account is called.
    fn my_sid() -> Result<String, String> {
        let out = quiet("whoami", &["/user", "/fo", "csv", "/nh"])?;
        let line = String::from_utf8_lossy(&out.stdout);
        line.trim()
            .rsplit(',')
            .next()
            .map(|s| s.trim_matches('"').to_owned())
            .filter(|s| s.starts_with("S-1-"))
            .ok_or_else(|| format!("whoami gave no SID: {}", line.trim()))
    }

    /// One argument as `CommandLineToArgvW` reads it back.
    fn quote(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            return arg.to_owned();
        }
        let mut out = String::from("\"");
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(slashes * 2 + 1));
                    out.push('"');
                    slashes = 0;
                }
                _ => {
                    out.push_str(&"\\".repeat(slashes));
                    out.push(c);
                    slashes = 0;
                }
            }
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out
    }

    /// Start `exe` with these arguments as an administrator — the consent
    /// dialog is Windows' — and wait for it. A refusal is an error.
    fn runas(exe: &Path, args: &[String]) -> Result<u32, String> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, INFINITE, WaitForSingleObject,
        };
        use windows::Win32::UI::Shell::{
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        use windows::core::{HSTRING, PCWSTR, w};
        let file = HSTRING::from(exe.as_os_str());
        let params = HSTRING::from(args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: every pointer in `info` outlives the call; the process handle
        // it returns is waited on and closed here.
        unsafe {
            ShellExecuteExW(&mut info).map_err(|e| format!("administrator rights: {e}"))?;
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
            Ok(code)
        }
    }

    /// Ask the service in this session to write its index down and leave,
    /// and wait until it has.
    fn stop_session(addr: &str) {
        if let Ok(mut c) = scour_ipc::Client::connect(addr) {
            let _ = c.call(scour_proto::Request::Shutdown {});
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while scour_ipc::is_running(addr) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn copy_files(from: &Path, to: &Path) -> Result<(), String> {
        std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
        for entry in std::fs::read_dir(from)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let path = entry.path();
            if path.is_file() {
                let dest = to.join(entry.file_name());
                std::fs::copy(&path, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            }
        }
        Ok(())
    }

    fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
        std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
        for entry in std::fs::read_dir(from)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let path = entry.path();
            let dest = to.join(entry.file_name());
            if path.is_dir() {
                copy_tree(&path, &dest)?;
            } else {
                std::fs::copy(&path, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            }
        }
        Ok(())
    }

    fn sc(args: &[&str]) -> Result<String, String> {
        let out = quiet("sc", args)?;
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            Ok(said)
        } else {
            Err(format!("sc {}: {}", args.join(" "), said.trim()))
        }
    }

    /// Stop the service if it runs, and wait until the manager says so.
    fn stop_service() {
        let _ = sc(&["stop", SERVICE]);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            match sc(&["query", SERVICE]) {
                Ok(said) if said.contains("RUNNING") || said.contains("PENDING") => {
                    std::thread::sleep(Duration::from_millis(300));
                }
                _ => return,
            }
        }
    }

    /// From the window: move to the service (`true`) or back to the session.
    /// On success the window to open next has been started; the caller leaves.
    pub fn switch(to_service: bool, config: &scour_config::Config) -> Result<(), String> {
        let addr = config.socket();
        let index = config.index.dir.display().to_string();
        if to_service {
            let src = here()?;
            let sid = my_sid()?;
            let state = config.state_dir().display().to_string();
            let cfg = scour_config::config_path();
            let cfg = if cfg.exists() {
                cfg.display().to_string()
            } else {
                String::new()
            };
            stop_session(&addr);
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            let args: Vec<String> = vec![
                "--service-on".into(),
                src.display().to_string(),
                sid,
                state,
                cfg,
                index,
                addr,
            ];
            let code = runas(&me, &args)?;
            if code != 0 {
                return Err(format!("the move into Program Files stopped (code {code})"));
            }
            let pf = program_files()?;
            let _ = crate::autostart::set(false);
            crate::install::adopt(&pf, &src)?;
            Command::new(pf.join("scour-gui.exe"))
                .current_dir(&pf)
                .spawn()
                .map_err(|e| e.to_string())?;
        } else {
            let pf = here()?;
            let user = crate::install::user_home()?;
            // The copy the session runs from is the person's to write.
            copy_files(&pf, &user)?;
            let args: Vec<String> = vec!["--service-off".into(), index];
            let code = runas(&user.join("scour-gui.exe"), &args)?;
            if code != 0 {
                return Err(format!("the move back stopped (code {code})"));
            }
            crate::install::adopt(&user, &pf)?;
            let _ = crate::autostart::set_for(&user.join("scour-gui.exe"), true);
            Command::new(user.join("scour-gui.exe"))
                .current_dir(&user)
                .spawn()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// As an administrator: programs to Program Files, the index to ProgramData
    /// behind an access list, and the service registered and started.
    /// `[source folder, owner SID, state dir, config file or "", index, pipe]`.
    pub fn elevated_on(v: &[String]) -> Result<(), String> {
        let [src, sid, state, config, index, addr] = v else {
            return Err("--service-on takes six values".into());
        };
        let pf = program_files()?;
        let pd = program_data()?;
        let registered = scour_launch::windows_service_registered();
        if registered {
            stop_service();
        }
        if !same(Path::new(src), &pf) {
            copy_files(Path::new(src), &pf)?;
        }
        std::fs::create_dir_all(&pd).map_err(|e| format!("{}: {e}", pd.display()))?;
        // SYSTEM and the administrators only: the index holds every file name
        // of every disk, and ProgramData lets every account read by default.
        let pds = pd.display().to_string();
        let out = quiet(
            "icacls",
            &[
                &pds,
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
            ],
        )?;
        if !out.status.success() {
            return Err(format!(
                "icacls: {}",
                String::from_utf8_lossy(&out.stdout).trim()
            ));
        }
        let pindex = pd.join("index");
        if !pindex.exists() && Path::new(index).is_dir() {
            // The session's index, so the service starts where it left off.
            copy_tree(Path::new(index), &pindex)?;
        }
        let mut bin = format!(
            "{} --service --owner {sid} --state {} --index {} --socket {addr}",
            quote(&pf.join("scourd.exe").display().to_string()),
            quote(state),
            quote(&pindex.display().to_string()),
        );
        if !config.is_empty() {
            bin.push_str(&format!(" --config {}", quote(config)));
        }
        if registered {
            sc(&["config", SERVICE, "binPath=", &bin, "start=", "auto"])?;
        } else {
            sc(&[
                "create",
                SERVICE,
                "binPath=",
                &bin,
                "start=",
                "auto",
                "DisplayName=",
                SERVICE,
            ])?;
        }
        let _ = sc(&[
            "description",
            SERVICE,
            "Keeps the Scour file index current and answers its searches.",
        ]);
        sc(&["start", SERVICE])?;
        Ok(())
    }

    /// As an administrator: the service stopped and removed, its index handed
    /// back to the person, ProgramData and Program Files cleared.
    /// `[the person's index folder]`.
    pub fn elevated_off(v: &[String]) -> Result<(), String> {
        let [index] = v else {
            return Err("--service-off takes one value".into());
        };
        stop_service();
        let _ = sc(&["delete", SERVICE]);
        let pd = program_data()?;
        let pindex = pd.join("index");
        if pindex.is_dir() {
            let _ = std::fs::remove_dir_all(index);
            copy_tree(&pindex, Path::new(index))?;
        }
        let _ = std::fs::remove_dir_all(&pd);
        // The window that asked still runs from Program Files: the folder goes
        // once it has left.
        let pf = program_files()?.display().to_string();
        let _ = Command::new("cmd")
            .raw_arg(format!(
                "/c ping -n 8 127.0.0.1 >nul & rmdir /s /q \"{pf}\""
            ))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        Ok(())
    }

    fn same(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::quote;

        #[test]
        fn arguments_are_quoted_as_windows_reads_them_back() {
            assert_eq!(quote("plain"), "plain");
            assert_eq!(quote(""), "\"\"");
            assert_eq!(
                quote(r"C:\Program Files\Scour"),
                r#""C:\Program Files\Scour""#
            );
            assert_eq!(quote(r"C:\a b\"), r#""C:\a b\\""#);
            assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        }
    }
}
