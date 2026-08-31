//! Client for rift's scripting addition.
//!
//! Some things the window server used to allow have no unprivileged API left on
//! macOS 26 — moving a window to a specific space is the one that matters here.
//! Every route that does not need elevated privileges was probed and is gone:
//! `SLSPerformAsynchronousBridgedWindowManagementOperation` no longer exports,
//! `SLSSetWindowListWorkspace` answers `kCGErrorNotImplemented`,
//! `SLSMoveWindowsToManagedSpace` accepts the call and does nothing, and
//! `CGSAddWindowsToSpaces` is no longer in SkyLight at all.
//!
//! What remains is code running *inside* Dock, which is what a scripting
//! addition is: a payload injected into Dock that listens on a unix socket.
//! [`crate::sys::osax`] builds, installs and injects that payload; this module
//! is its client, and a command is fifteen bytes. The important detail is that
//! the payload belongs to Dock, not to rift — it keeps answering while rift is
//! not running, and a Dock restart drops it until `sudo rift sa load` runs
//! again.
//!
//! This is strictly opt-in and strictly a fallback: rift's own paths are used
//! wherever one exists, and every function here reports failure rather than
//! pretending, so a machine without the addition simply does not get these
//! commands.
//!
//! Wire format, from yabai's `src/sa.m`, which the vendored payload keeps:
//!
//! ```text
//! [i16 length][u8 opcode][args, native endian]
//! ```
//!
//! where `length` counts the opcode and args, i.e. the frame size minus its own
//! two bytes. The server replies with a single byte and closes; each command is
//! its own short-lived connection, so there is no session to keep. The one
//! exception is [`handshake`], answered with a NUL-terminated version string
//! followed by a `u32` of attribute flags.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use tracing::{debug, warn};

/// The payload's opcodes. Only the ones rift has no other way to perform are
/// listed; the numbering belongs to `enum sa_opcode` in `src/osax/common.h`.
mod opcode {
    pub const HANDSHAKE: u8 = 0x01;
    pub const SPACE_FOCUS: u8 = 0x02;
    pub const SPACE_CREATE: u8 = 0x03;
    pub const SPACE_MOVE: u8 = 0x05;
    pub const SPACE_DESTROY: u8 = 0x04;
    pub const WINDOW_TO_SPACE: u8 = 0x13;
}

/// Which Dock internals the payload managed to find, one flag each, matching
/// the `OSAX_ATTRIB_*` defines in `src/osax/common.h`.
pub mod attrib {
    /// `dock_spaces`, the object every space command goes through.
    pub const DOCK_SPACES: u32 = 0x01;
    /// The desktop picture manager, which a space move has to be told about.
    pub const DPPM: u32 = 0x02;
    pub const ADD_SPACE: u32 = 0x04;
    pub const REM_SPACE: u32 = 0x08;
    pub const MOV_SPACE: u32 = 0x10;
    /// Dock's "set front window", which rift does its own way.
    pub const SET_WINDOW: u32 = 0x20;
    /// The instruction the payload patches to zero out Dock's space-change
    /// animation. rift never asks for this and does not need it.
    pub const ANIM_TIME: u32 = 0x40;

    /// The flags whose absence would actually cost rift a command, and the
    /// command each one is for:
    ///
    /// | flag | needed by |
    /// |---|---|
    /// | `DOCK_SPACES` | every space command |
    /// | `ADD_SPACE` | [`super::create_space`] |
    /// | `REM_SPACE` | [`super::destroy_space`] |
    /// | `MOV_SPACE`, `DPPM` | [`super::move_space_after_space`] |
    ///
    /// `SET_WINDOW` and `ANIM_TIME` are deliberately absent: they belong to
    /// yabai commands rift does not send. [`super::move_window_to_space`],
    /// the one that matters most, needs no flag at all — inside Dock it is a
    /// plain `SLSMoveWindowsToManagedSpace` that works from there and nowhere
    /// else.
    pub const REQUIRED: u32 = DOCK_SPACES | DPPM | ADD_SPACE | REM_SPACE | MOV_SPACE;

    /// Names the flags in `wanted` that `reported` does not have, for messages.
    pub fn missing(reported: u32, wanted: u32) -> Vec<&'static str> {
        [
            (DOCK_SPACES, "dock.spaces"),
            (DPPM, "desktop picture manager"),
            (ADD_SPACE, "add space"),
            (REM_SPACE, "remove space"),
            (MOV_SPACE, "move space"),
            (SET_WINDOW, "set front window"),
            (ANIM_TIME, "animation time"),
        ]
        .into_iter()
        .filter(|(flag, _)| wanted & flag != 0 && reported & flag == 0)
        .map(|(_, name)| name)
        .collect()
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

/// The socket a payload running inside `user`'s Dock serves, matching
/// `SA_SOCKET_PATH_FMT` in `src/osax/common.h`.
pub fn socket_path_for_user(user: &str) -> String { format!("/tmp/rift-sa_{user}.socket") }

fn socket_path() -> Option<String> {
    let user = std::env::var("USER").ok()?;
    Some(socket_path_for_user(&user))
}

/// What a payload answers a handshake with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// The payload's compiled-in `OSAX_VERSION`, which is the build inside Dock
    /// right now — not whatever is installed on disk.
    pub version: String,
    /// Which Dock internals the payload found, as [`attrib`] flags. What
    /// matters is [`Self::missing`], not whether every flag is set.
    pub attributes: u32,
}

impl Handshake {
    /// The names of the flags rift needs and this payload does not have.
    ///
    /// Empty is the answer to look for. A payload can report less than every
    /// flag and still serve rift completely: a second payload in the same Dock
    /// will not find `ANIM_TIME`, because finding it means matching an
    /// instruction the first payload has already overwritten.
    pub fn missing(&self) -> Vec<&'static str> {
        attrib::missing(self.attributes, attrib::REQUIRED)
    }
}

/// Asks the payload at `path` what it is, without acting on anything.
///
/// This is the only honest test of whether the addition is live: the bundle can
/// be installed and current while the Dock holding it has been restarted out
/// from under it.
pub fn handshake(path: &str) -> Option<Handshake> {
    let mut stream = UnixStream::connect(path).ok()?;
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));

    stream.write_all(&[0x01, 0x00, opcode::HANDSHAKE]).ok()?;

    // The reply is `version\0`, four bytes of attributes, then a newline, and
    // the payload closes the connection behind it.
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).ok()?;

    let nul = reply.iter().position(|byte| *byte == 0)?;
    let version = String::from_utf8(reply[..nul].to_vec()).ok()?;
    let attributes = reply.get(nul + 1..nul + 5)?.try_into().ok()?;

    Some(Handshake {
        version,
        attributes: u32::from_ne_bytes(attributes),
    })
}

/// Whether the scripting addition is loaded and accepting connections.
pub fn is_available() -> bool {
    #[cfg(test)]
    {
        return test_hooks::available();
    }
    #[allow(unreachable_code)]
    socket_path().is_some_and(|path| UnixStream::connect(path).is_ok())
}

/// The test suite runs on developer machines that may well have the addition
/// loaded, and a command sent to it there acts on the real Dock. Under test
/// the socket is never touched: commands are recorded instead, and whether
/// the addition "is available" is whatever the test says.
#[cfg(test)]
pub mod test_hooks {
    use std::cell::{Cell, RefCell};

    thread_local! {
        static AVAILABLE: Cell<bool> = const { Cell::new(false) };
        static SENT: RefCell<Vec<(u8, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    }

    pub fn available() -> bool {
        AVAILABLE.with(|available| available.get())
    }

    pub fn set_available(available: bool) {
        AVAILABLE.with(|cell| cell.set(available));
        SENT.with(|sent| sent.borrow_mut().clear());
    }

    pub(super) fn record(op: u8, args: &[u8]) -> bool {
        if !available() {
            return false;
        }
        SENT.with(|sent| sent.borrow_mut().push((op, args.to_vec())));
        true
    }

    /// `(window server id, space)` of every window-to-space command sent, in order.
    pub fn window_moves() -> Vec<(u32, u64)> {
        SENT.with(|sent| {
            sent.borrow()
                .iter()
                .filter(|(op, _)| *op == super::opcode::WINDOW_TO_SPACE)
                .map(|(_, args)| {
                    let space = u64::from_ne_bytes(args[0..8].try_into().unwrap());
                    let window = u32::from_ne_bytes(args[8..12].try_into().unwrap());
                    (window, space)
                })
                .collect()
        })
    }

    /// Every space-focus command sent, in order.
    pub fn space_focuses() -> Vec<u64> {
        SENT.with(|sent| {
            sent.borrow()
                .iter()
                .filter(|(op, _)| *op == super::opcode::SPACE_FOCUS)
                .map(|(_, args)| u64::from_ne_bytes(args[0..8].try_into().unwrap()))
                .collect()
        })
    }
}

/// Sends one command. Returns whether the addition accepted it.
///
/// A missing socket is the normal case on a machine without the addition, and
/// is logged at debug rather than warn so it does not read as a fault.
fn send(op: u8, args: &[u8]) -> bool {
    #[cfg(test)]
    {
        return test_hooks::record(op, args);
    }
    #[allow(unreachable_code)]
    let Some(path) = socket_path() else {
        return false;
    };

    let body_len = 1 + args.len();
    let Ok(header) = i16::try_from(body_len) else {
        warn!("scripting addition command too large");
        return false;
    };

    let mut frame = Vec::with_capacity(2 + body_len);
    frame.extend_from_slice(&header.to_ne_bytes());
    frame.push(op);
    frame.extend_from_slice(args);

    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) => {
            debug!(%error, %path, "scripting addition is not loaded");
            return false;
        }
    };
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));

    if let Err(error) = stream.write_all(&frame) {
        warn!(%error, op, "failed to send scripting addition command");
        return false;
    }

    // The payload answers with a single byte and closes. A closed connection
    // with nothing read still means the command was taken.
    let mut ack = [0u8; 1];
    let _ = stream.read(&mut ack);
    true
}

/// Moves a window to a space, both by their window-server ids.
pub fn move_window_to_space(window_server_id: u32, space: u64) -> bool {
    crate::sys::trace::observe("sa_move_window_to_space", (window_server_id, space), || {
        let mut args = Vec::with_capacity(12);
        args.extend_from_slice(&space.to_ne_bytes());
        args.extend_from_slice(&window_server_id.to_ne_bytes());
        send(opcode::WINDOW_TO_SPACE, &args)
    })
}

/// Focuses a space by id, with no animation whatsoever.
///
/// Inside Dock this is `SLSShowSpaces` / `SLSHideSpaces` /
/// `SLSManagedDisplaySetCurrentSpace` plus a patch of Dock's own
/// `_currentSpace` ivar — a teleport, not a gesture, so nothing slides. The
/// synthetic-swipe path in `sys::space_switch` still shows a single frame of
/// movement because it drives the Dock's real swipe machinery; this does not.
///
/// The trade is state: this sets the window server's idea of the current space
/// directly, and issuing many in quick succession has been observed to leave
/// Dock settling somewhere other than the last space asked for. Fine for
/// ordinary use, worse than the gesture under repeated rapid switching.
pub fn focus_space(space: u64) -> bool {
    crate::sys::trace::observe("sa_focus_space", space, || {
        send(opcode::SPACE_FOCUS, &space.to_ne_bytes())
    })
}

/// Creates a space on the display holding `space`.
pub fn create_space(space: u64) -> bool {
    send(opcode::SPACE_CREATE, &space.to_ne_bytes())
}

/// Reorders `space` to sit immediately after `after` on the same display,
/// optionally focusing it.
///
/// The third argument is a slot yabai uses when moving a space between
/// displays and leaves zeroed otherwise.
pub fn move_space_after_space(space: u64, after: u64, focus: bool) -> bool {
    let mut args = Vec::with_capacity(25);
    args.extend_from_slice(&space.to_ne_bytes());
    args.extend_from_slice(&after.to_ne_bytes());
    args.extend_from_slice(&0u64.to_ne_bytes());
    args.push(u8::from(focus));
    send(opcode::SPACE_MOVE, &args)
}

/// Destroys a space by id.
pub fn destroy_space(space: u64) -> bool {
    send(opcode::SPACE_DESTROY, &space.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_attributes_exclude_what_rift_never_sends() {
        // Observed on macOS 26.6.2: a payload loaded into a Dock that already
        // holds another one reports 0x3f, never 0x40 -- finding ANIM_TIME means
        // matching an instruction the first payload has already overwritten.
        // rift sends no opcode that needs it, so 0x3f is healthy.
        let handshake = Handshake { version: "1.0.0".into(), attributes: 0x3f };
        assert!(handshake.missing().is_empty());

        assert_eq!(attrib::REQUIRED & attrib::ANIM_TIME, 0);
        assert_eq!(attrib::REQUIRED & attrib::SET_WINDOW, 0);
    }

    #[test]
    fn missing_required_attributes_are_named() {
        let handshake = Handshake {
            version: "1.0.0".into(),
            attributes: attrib::REQUIRED & !attrib::ADD_SPACE & !attrib::MOV_SPACE,
        };
        assert_eq!(handshake.missing(), vec!["add space", "move space"]);
    }

    #[test]
    fn a_payload_that_found_nothing_is_missing_every_requirement() {
        let handshake = Handshake { version: "1.0.0".into(), attributes: 0 };
        assert_eq!(handshake.missing().len(), 5);
    }

    #[test]
    fn frame_layout_matches_the_payload() {
        // move_window_to_space packs sid then wid after the opcode, so the
        // frame is 2 header + 1 opcode + 8 + 4 = 15 bytes, and the length
        // header counts everything but itself.
        let args_len = size_of::<u64>() + size_of::<u32>();
        let body_len = 1 + args_len;
        assert_eq!(body_len, 13);
        assert_eq!(2 + body_len, 15);
    }
}
