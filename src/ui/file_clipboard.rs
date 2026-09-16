//! File clipboard ownership, including Linux file-manager MIME formats.
use std::{path::PathBuf, sync::mpsc, thread, time::Duration};

pub(super) struct FileClipboard {
    _keep_alive: mpsc::Sender<()>,
}
impl FileClipboard {
    pub fn copy(path: PathBuf) -> (Self, mpsc::Receiver<Result<(), String>>) {
        let (keep_alive, lifetime) = mpsc::channel();
        let (result, rx) = mpsc::channel();
        thread::spawn(move || {
            if let Err(error) = own_clipboard(path, lifetime, &result) {
                let _ = result.send(Err(error));
            }
        });
        (
            Self {
                _keep_alive: keep_alive,
            },
            rx,
        )
    }
}

#[cfg(target_os = "linux")]
fn own_clipboard(
    path: PathBuf,
    lifetime: mpsc::Receiver<()>,
    result: &mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    use x11rb::{
        connection::Connection,
        protocol::{Event, xproto::*},
        wrapper::ConnectionExt as _,
    };
    let run = || -> Result<(), Box<dyn std::error::Error>> {
        let (connection, screen) = x11rb::connect(None)?;
        let window = connection.generate_id()?;
        connection.create_window(
            0,
            window,
            connection.setup().roots[screen].root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new(),
        )?;
        let atom = |name: &str| -> Result<u32, Box<dyn std::error::Error>> {
            Ok(connection
                .intern_atom(false, name.as_bytes())?
                .reply()?
                .atom)
        };
        let clipboard = atom("CLIPBOARD")?;
        let targets = atom("TARGETS")?;
        let uri = atom("text/uri-list")?;
        let gnome = atom("x-special/gnome-copied-files")?;
        let utf8 = atom("UTF8_STRING")?;
        let absolute = path.canonicalize()?;
        let uri_text = file_uri(&absolute);
        let text = absolute.to_string_lossy().into_owned();
        connection
            .set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)?
            .check()?;
        connection.flush()?;
        if connection.get_selection_owner(clipboard)?.reply()?.owner != window {
            return Err("Could not acquire file clipboard".into());
        }
        let _ = result.send(Ok(()));
        while matches!(lifetime.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            while let Some(event) = connection.poll_for_event()? {
                match event {
                    Event::SelectionClear(_) => return Ok(()),
                    Event::SelectionRequest(request) => {
                        let property = if request.property == 0 {
                            request.target
                        } else {
                            request.property
                        };
                        let supported = if request.target == targets {
                            connection.change_property32(
                                PropMode::REPLACE,
                                request.requestor,
                                property,
                                AtomEnum::ATOM,
                                &[targets, uri, gnome, utf8],
                            )?;
                            true
                        } else {
                            let bytes = if request.target == uri {
                                Some(format!("{uri_text}\r\n"))
                            } else if request.target == gnome {
                                Some(format!("copy\n{uri_text}"))
                            } else if request.target == utf8 {
                                Some(text.clone())
                            } else {
                                None
                            };
                            if let Some(bytes) = bytes {
                                connection.change_property8(
                                    PropMode::REPLACE,
                                    request.requestor,
                                    property,
                                    request.target,
                                    bytes.as_bytes(),
                                )?;
                                true
                            } else {
                                false
                            }
                        };
                        connection.send_event(
                            false,
                            request.requestor,
                            EventMask::NO_EVENT,
                            SelectionNotifyEvent {
                                response_type: SELECTION_NOTIFY_EVENT,
                                sequence: 0,
                                time: request.time,
                                requestor: request.requestor,
                                selection: request.selection,
                                target: request.target,
                                property: if supported { property } else { 0 },
                            },
                        )?;
                        connection.flush()?;
                    }
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    };
    run().map_err(|e| e.to_string())
}
#[cfg(not(target_os = "linux"))]
fn own_clipboard(
    _: PathBuf,
    _: mpsc::Receiver<()>,
    _: &mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    Err("File copying is not supported on this platform yet".into())
}
#[cfg(target_os = "linux")]
fn file_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an isolated X11 display"]
    fn file_clipboard_serves_uri_and_file_manager_formats() {
        use x11rb::{
            connection::Connection,
            protocol::{Event, xproto::*},
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip 日本.mp4");
        std::fs::write(&path, b"fixture").unwrap();
        let (_owner, result) = FileClipboard::copy(path.clone());
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let (connection, screen) = x11rb::connect(None).unwrap();
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                window,
                connection.setup().roots[screen].root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new(),
            )
            .unwrap();
        let atom = |name: &str| {
            connection
                .intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let clipboard = atom("CLIPBOARD");
        let property = atom("SLICER_TEST_CLIPBOARD");
        for (mime, expected) in [
            ("text/uri-list", format!("{}\r\n", file_uri(&path))),
            (
                "x-special/gnome-copied-files",
                format!("copy\n{}", file_uri(&path)),
            ),
        ] {
            let target = atom(mime);
            connection
                .convert_selection(window, clipboard, target, property, x11rb::CURRENT_TIME)
                .unwrap();
            connection.flush().unwrap();
            let began = std::time::Instant::now();
            loop {
                if let Some(Event::SelectionNotify(event)) = connection.poll_for_event().unwrap() {
                    assert_eq!(event.property, property);
                    break;
                }
                assert!(began.elapsed() < Duration::from_secs(2));
                thread::sleep(Duration::from_millis(10));
            }
            let reply = connection
                .get_property(true, window, property, target, 0, 4096)
                .unwrap()
                .reply()
                .unwrap();
            assert_eq!(reply.value, expected.as_bytes());
        }
    }
    #[test]
    fn file_uri_escapes_spaces_unicode_and_separators() {
        assert_eq!(
            file_uri(std::path::Path::new("/tmp/a #日本.mp4")),
            "file:///tmp/a%20%23%E6%97%A5%E6%9C%AC.mp4"
        );
    }
}
