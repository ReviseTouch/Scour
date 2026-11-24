//! Routes that act on files: open, open with, reveal, rename, send, trash,
//! delete, drag — each fenced by what the index says the path is.

use super::*;
use scour_client::Pool;
use scour_core::Owner;
use scour_proto::{Request, Response};
use std::net::TcpStream;

/// Start the window or the terminal, once a person has asked for it. Two
/// names, not a path: a program name from the page would run anything.
pub(super) fn api_face(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let which = req.param("face").unwrap_or_default();
    // Two names only: a program name from the page would run anything. The
    // terminal needs a tty, which `scour_launch::face` knows how to give.
    if !matches!(which, "window" | "tui") {
        http::fail(stream, "400 Bad Request", "no such face");
        return;
    }
    let Some(mut command) = scour_launch::face(which) else {
        http::fail(
            stream,
            "404 Not Found",
            &format!("{which} is not installed"),
        );
        return;
    };
    match command.spawn() {
        Ok(_) => {
            // Switching is also choosing: the desktop entry opens the last
            // face moved to. Best effort — the face is already up.
            if let Ok(change) = serde_json::from_value(serde_json::json!({ "face": which })) {
                let _ = call(client, Request::SetSettings { change });
            }
            http::respond(
                stream,
                "200 OK",
                "application/json",
                b"{\"started\":true,\"closing\":true}",
            );
            // And this face closes: switching is moving, not opening a second
            // one. A tab cannot close itself, so the server goes and the page
            // says the tab can be shut — after a beat, because a process that
            // exits while its child is still starting takes the child with it.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(700));
                std::process::exit(0);
            });
        }
        Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
    }
}

/// The first `name` beside this program, then on the `PATH`.
fn beside_or_path(name: &str) -> Option<std::path::PathBuf> {
    if let Ok(here) = std::env::current_exe()
        && let Some(dir) = here.parent()
    {
        let beside = dir.join(name);
        if beside.is_file() {
            return Some(beside);
        }
        let script = dir.join("../../scripts").join(name);
        if script.is_file() {
            return Some(script);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Which programs on this machine will take this file. A read, so a `GET`; the
/// desktop's own association is marked rather than moved.
pub(super) fn api_openers(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let Some(path) = req.param("path").filter(|p| !p.is_empty()) else {
        http::fail(stream, "400 Bad Request", "no path");
        return;
    };
    if !matches!(
        call(
            client,
            Request::Stat {
                path: path.to_string()
            }
        ),
        Ok(Response::Stat(_))
    ) {
        http::fail(stream, "404 Not Found", "not in the index");
        return;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let mime = scour_thumbs::known::known().mime_of(name).unwrap_or("");
    let list: Vec<serde_json::Value> = scour_openers::openers(mime)
        .into_iter()
        .map(|o| serde_json::json!({ "id": o.id, "name": o.name, "preferred": o.preferred }))
        .collect();
    http::json(stream, &serde_json::json!({ "openers": list }));
}

/// Start one of them, behind `--no-launch` with everything else that runs a
/// program. The list above is a read and stays available.
pub(super) fn api_open_with(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let (Some(path), Some(id)) = (
        req.param("path").filter(|p| !p.is_empty()),
        req.param("id").filter(|p| !p.is_empty()),
    ) else {
        http::fail(stream, "400 Bad Request", "no path or no id");
        return;
    };
    if !matches!(
        call(
            client,
            Request::Stat {
                path: path.to_string()
            }
        ),
        Ok(Response::Stat(_))
    ) {
        http::fail(stream, "404 Not Found", "not in the index");
        return;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let mime = scour_thumbs::known::known().mime_of(name).unwrap_or("");
    match scour_openers::openers(mime)
        .into_iter()
        .find(|o| o.id == id)
    {
        Some(chosen) => match scour_openers::launch(&chosen, &scour_core::path::to_path(path)) {
            Ok(()) => http::json(stream, &serde_json::json!({ "started": chosen.name })),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        // The list the page was shown is a moment old.
        None => http::fail(stream, "404 Not Found", "no such program any more"),
    }
}

/// Give a file a different name, and tell the index straight away. Renameable
/// only if the index holds the path; what a name may be is `scour-name`'s.
pub(super) fn api_rename(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let (Some(path), Some(name)) = (
        req.param("path").filter(|p| !p.is_empty()),
        req.param("name"),
    ) else {
        http::fail(stream, "400 Bad Request", "no path or no name");
        return;
    };
    if !matches!(
        call(
            client,
            Request::Stat {
                path: path.to_owned()
            }
        ),
        Ok(Response::Stat(_))
    ) {
        http::fail(stream, "404 Not Found", "not in the index");
        return;
    }
    match scour_name::rename(&scour_core::path::to_path(path), name) {
        Ok(now) => {
            let now = scour_core::path::from_path(&now);
            // Both ends: the old path is gone and the new one has appeared.
            let _ = call(
                client,
                Request::Recheck {
                    paths: vec![path.to_owned(), now.clone()],
                },
            );
            http::json(stream, &serde_json::json!({ "path": now }));
        }
        // The refusal is a catalogue key, looked up in the reader's language.
        Err(why) => http::json(stream, &serde_json::json!({ "refused": why.msgid() })),
    }
}

/// Send rows to the trash, and tell the index straight away. Trashable
/// only if the index holds the path; the move is this process's, with this
/// user's permissions, and [`Request::Recheck`] only re-reads.
pub(super) fn api_trash(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    if asked.is_empty() {
        http::fail(stream, "400 Bad Request", "no paths");
        return;
    }

    let mut gone = Vec::new();
    let mut refused = Vec::new();
    for path in &asked {
        // In the index, or not ours.
        if !matches!(
            call(client, Request::Stat { path: path.clone() }),
            Ok(Response::Stat(_))
        ) {
            refused.push(format!("{path}: not in the index"));
            continue;
        }
        match scour_trash::trash(&scour_core::path::to_path(path)) {
            Ok(_) => gone.push(path.clone()),
            Err(e) => refused.push(format!("{path}: {e}")),
        }
    }

    // A failure is worth rechecking too: sometimes it is already gone.
    if !asked.is_empty() {
        let _ = call(client, Request::Recheck { paths: asked });
    }

    http::json(
        stream,
        &serde_json::json!({ "gone": gone.len(), "refused": refused }),
    );
}

/// The paths a request carries: a JSON array of strings in its body. Not one
/// a line — a name may hold a newline, and `victim\nsuffix.txt` split there
/// sent `victim` to the trash.
pub(super) fn paths_in(req: &http::Req) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&req.body)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect()
}

/// Those paths, kept only where the index knows them: the fence every
/// endpoint that changes the disk stands behind.
fn fenced(client: &Pool, asked: &[String]) -> (Vec<String>, Vec<String>) {
    let mut known = Vec::new();
    let mut refused = Vec::new();
    for path in asked {
        if matches!(
            call(
                client,
                Request::Stat {
                    path: path.to_owned()
                }
            ),
            Ok(Response::Stat(_))
        ) {
            known.push(path.to_owned());
        } else {
            refused.push(format!("{path}: not in the index"));
        }
    }
    (known, refused)
}

/// Remove for good. The page asked first; this only does it, and tells the
/// service to look again as the trash does.
pub(super) fn api_delete(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    if asked.is_empty() {
        http::fail(stream, "400 Bad Request", "no paths");
        return;
    }
    let (known, mut refused) = fenced(client, &asked);
    let mut gone = 0usize;
    for path in &known {
        match scour_trash::erase(&scour_core::path::to_path(path)) {
            Ok(()) => gone += 1,
            Err(e) => refused.push(format!("{path}: {e}")),
        }
    }
    if !known.is_empty() {
        let _ = call(client, Request::Recheck { paths: known });
    }
    http::json(
        stream,
        &serde_json::json!({ "gone": gone, "refused": refused }),
    );
}

/// Show these in the file manager, each selected in its folder.
pub(super) fn api_reveal(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    if asked.is_empty() {
        http::fail(stream, "400 Bad Request", "no paths");
        return;
    }
    let (known, refused) = fenced(client, &asked);
    let native: Vec<std::path::PathBuf> =
        known.iter().map(|p| scour_core::path::to_path(p)).collect();
    let paths: Vec<&std::path::Path> = native.iter().map(std::path::PathBuf::as_path).collect();
    scour_openers::reveal(&paths);
    http::json(
        stream,
        &serde_json::json!({ "shown": known.len(), "refused": refused }),
    );
}

/// Where a selection can be sent on this machine now.
pub(super) fn api_send_targets(stream: &mut TcpStream) {
    let targets: Vec<serde_json::Value> = scour_sendto::targets()
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "label": t.label,
                "named": t.named,
                "files_only": t.files_only,
            })
        })
        .collect();
    http::json(stream, &serde_json::json!({ "targets": targets }));
}

/// Send what the index knows of these to one of the places above.
pub(super) fn api_send(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    let Some(to) = req
        .param("to")
        .filter(|t| !t.is_empty() && !asked.is_empty())
    else {
        http::fail(stream, "400 Bad Request", "no paths or no target");
        return;
    };
    let (known, refused) = fenced(client, &asked);
    if known.is_empty() {
        http::json(stream, &serde_json::json!({ "error": refused.join("\n") }));
        return;
    }
    let native: Vec<std::path::PathBuf> =
        known.iter().map(|p| scour_core::path::to_path(p)).collect();
    let paths: Vec<&std::path::Path> = native.iter().map(std::path::PathBuf::as_path).collect();
    let answer = match scour_sendto::send(to, &paths) {
        Ok(scour_sendto::Sent::Linked(at)) => {
            serde_json::json!({ "sent": "linked", "at": at.first().map(|p| p.to_string_lossy()) })
        }
        Ok(scour_sendto::Sent::Handed) => serde_json::json!({ "sent": "handed" }),
        Ok(scour_sendto::Sent::Packing(at)) => {
            serde_json::json!({ "sent": "packing", "at": at.to_string_lossy() })
        }
        Ok(scour_sendto::Sent::Copying(at)) => {
            serde_json::json!({ "sent": "copying", "at": at.to_string_lossy() })
        }
        Err(e) => serde_json::json!({ "error": e.msgid(), "detail": e.to_string() }),
    };
    http::json(stream, &answer);
}

/// Open the window's drag tray for these paths. A page cannot drag a file out:
/// Chromium hands its `file://` on as `about:blank#`, and `DownloadURL` is not
/// read on Linux. The tray is a window of its own, whose drag is real.
pub(super) fn api_drag(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    let (known, refused) = fenced(client, &asked);
    if known.is_empty() {
        http::json(stream, &serde_json::json!({ "error": refused.join("\n") }));
        return;
    }
    let Some(binary) = beside_or_path("scour-gui") else {
        http::json(
            stream,
            &serde_json::json!({ "error": "scour-gui is not installed" }),
        );
        return;
    };
    let mut command = std::process::Command::new(&binary);
    command.arg("--drag").args(&known);
    // Its own process group, or closing this server closes the tray.
    scour_ui::faces::detach(&mut command);
    match command.spawn() {
        Ok(_) => http::json(stream, &serde_json::json!({ "opened": known.len() })),
        Err(e) => http::json(stream, &serde_json::json!({ "error": e.to_string() })),
    }
}

/// Open a path, or the folder holding it. The index is the fence: the service
/// is asked first, so this cannot be pointed at `/etc/shadow`. An executable
/// is shown selected in its folder — a double-click in a downloaded folder
/// must not start what it lands on — unless the menu's "Open" asked to run it
/// (`ask=1`), and then only once the page has asked and been told yes, and
/// never under `--no-run`.
pub(super) fn api_open(stream: &mut TcpStream, client: &Pool, req: &http::Req, may_run: bool) {
    let Some(path) = req.param("path").filter(|p| !p.is_empty()) else {
        http::fail(stream, "400 Bad Request", "no path");
        return;
    };
    let entry = match call(
        client,
        Request::Stat {
            path: path.to_owned(),
        },
    ) {
        Ok(Response::Stat(e)) => e,
        Ok(_) => {
            http::fail(stream, "502 Bad Gateway", "unexpected reply");
            return;
        }
        // Not in the index, or under no configured root: not ours to open.
        Err(e) => {
            http::fail(stream, "404 Not Found", &e);
            return;
        }
    };

    let native = scour_core::path::to_path(&entry.path);
    let p = native.as_path();

    // The desktop's own quick-look: a viewer rather than the handler the
    // extension names, which is why it is its own verb rather than a heuristic.
    if req.param("what") == Some("preview")
        && let Some(cmd) = QUICKLOOK.get().and_then(|q| q.as_ref())
    {
        let mut c = std::process::Command::new(&cmd[0]);
        c.args(&cmd[1..]).arg(p);
        match c
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => http::json(
                stream,
                &serde_json::json!({ "opened": entry.path, "with": cmd[0] }),
            ),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        }
        return;
    }

    // "Its folder" is the folder with this one selected in it: opening the
    // folder alone did nothing when it was already open, and showed nothing.
    if req.param("what") == Some("folder") {
        scour_openers::reveal(&[p]);
        http::json(
            stream,
            &serde_json::json!({ "opened": p.parent().unwrap_or(p).to_string_lossy() }),
        );
        return;
    }
    // A directory opened is that directory.
    let want_folder = entry.is_dir;
    // Asked of the entry, not the disk: every file on an `ntfs3` mount with
    // `fmask=0022` is 0755, so the mode bit alone would try to run a PDF.
    let runnable = !want_folder && scour_core::runs_when_opened(entry.name(), entry.meta.mode);
    let asked = req.param("ask") == Some("1") || req.param("run") == Some("yes");
    let run = runnable && may_run && asked;
    if run && req.param("run") != Some("yes") {
        http::json(stream, &serde_json::json!({ "ask_run": entry.name() }));
        return;
    }
    // Not run: shown selected in its folder, as the window shows it.
    if runnable && !run {
        scour_openers::reveal(&[p]);
        http::json(
            stream,
            &serde_json::json!({
                "opened": p.parent().unwrap_or(p).to_string_lossy(),
                "folder_instead": true,
            }),
        );
        return;
    }
    let target = p.to_path_buf();

    // Started in the directory it lives in, beside what it reads.
    let started = if run {
        std::process::Command::new(&target)
            .current_dir(target.parent().unwrap_or(&target))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
    } else {
        scour_openers::open(target.as_os_str())
    };
    match started {
        Ok(_) => http::json(
            stream,
            &serde_json::json!({
                "opened": target.to_string_lossy(),
                // Said, because nothing else on screen will say it was run;
                // the page words it, in the reader's language.
                "ran": run.then(|| target.file_name().unwrap_or_default().to_string_lossy()),
            }),
        ),
        Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
    }
}

/// The name behind a numeric id, from this machine.
pub(super) fn owner_name(which: Owner, id: i64) -> String {
    scour_core::owner_name(which, id)
}
