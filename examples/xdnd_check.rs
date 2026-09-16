//! Sends a real X11 file drag to a disposable Slicer test window.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use x11rb::{
        connection::Connection,
        protocol::{Event, xproto::*},
        wrapper::ConnectionExt as _,
    };
    let args: Vec<_> = std::env::args().collect();
    let target = u32::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
    let path = std::path::Path::new(&args[2]).canonicalize()?;
    let (c, screen) = x11rb::connect(None)?;
    let source = c.generate_id()?;
    c.create_window(
        0,
        source,
        c.setup().roots[screen].root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    )?;
    let atom = |s: &str| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(c.intern_atom(false, s.as_bytes())?.reply()?.atom)
    };
    let selection = atom("XdndSelection")?;
    let uri = atom("text/uri-list")?;
    let action = atom("XdndActionCopy")?;
    let plain = atom("text/plain")?;
    let mixed = args.iter().any(|a| a == "mixed");
    let type_list = args.iter().any(|a| a == "type-list");
    let delayed = args.iter().any(|a| a == "delayed");
    c.set_selection_owner(source, selection, x11rb::CURRENT_TIME)?;
    let send = |kind: &str, data: [u32; 5]| -> Result<(), Box<dyn std::error::Error>> {
        c.send_event(
            false,
            target,
            EventMask::NO_EVENT,
            ClientMessageEvent::new(32, target, atom(kind)?, data),
        )?;
        c.flush()?;
        Ok(())
    };
    // Deliberately below the logical window bounds at 2x scaling. GPUI's old
    // element hitbox path missed these device-pixel coordinates.
    c.warp_pointer(x11rb::NONE, target, 0, 0, 0, 0, 1000, 1000)?;
    if type_list {
        c.change_property32(
            PropMode::REPLACE,
            source,
            atom("XdndTypeList")?,
            AtomEnum::ATOM,
            &[plain, atom("UTF8_STRING")?, uri],
        )?;
    }
    send(
        "XdndEnter",
        [
            source,
            (5 << 24) | u32::from(type_list),
            if mixed { plain } else { uri },
            if mixed { uri } else { 0 },
            0,
        ],
    )?;
    send("XdndPosition", [source, 0, 0, x11rb::CURRENT_TIME, action])?;
    if delayed {
        send("XdndDrop", [source, 0, x11rb::CURRENT_TIME, 0, 0])?;
    }
    let began = Instant::now();
    let mut sent = delayed;
    while began.elapsed() < Duration::from_secs(9) {
        while let Some(event) = c.poll_for_event()? {
            if let Event::SelectionRequest(r) = event {
                if delayed {
                    thread::sleep(Duration::from_millis(150));
                }
                let data = format!(
                    "file://{}\r\n",
                    path.to_str()
                        .unwrap()
                        .replace('%', "%25")
                        .replace(' ', "%20")
                );
                let data = if r.target == plain {
                    path.display().to_string()
                } else {
                    data
                };
                println!("requested_target={} uri={} plain={}", r.target, uri, plain);
                c.change_property8(
                    PropMode::REPLACE,
                    r.requestor,
                    r.property,
                    r.target,
                    data.as_bytes(),
                )?;
                c.send_event(
                    false,
                    r.requestor,
                    EventMask::NO_EVENT,
                    SelectionNotifyEvent {
                        response_type: SELECTION_NOTIFY_EVENT,
                        sequence: 0,
                        time: r.time,
                        requestor: r.requestor,
                        selection: r.selection,
                        target: r.target,
                        property: r.property,
                    },
                )?;
                c.flush()?;
            }
        }
        if !sent && began.elapsed() > Duration::from_secs(6) {
            send(
                if args.get(3).is_some_and(|v| v == "leave") {
                    "XdndLeave"
                } else {
                    "XdndDrop"
                },
                [source, 0, x11rb::CURRENT_TIME, 0, 0],
            )?;
            sent = true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {}
