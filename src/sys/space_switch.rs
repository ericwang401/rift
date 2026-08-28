// references:
// https://github.com/mgbowen/FasterSwiper
// https://github.com/jurplel/InstantSpaceSwitcher/issues/72

use std::mem::size_of;

use dispatchr::queue;
use dispatchr::time::Time;
use objc2_core_foundation::{CFData, CFRetained};
use objc2_core_graphics::{CGEvent, CGEventField};
use objc2_foundation::NSProcessInfo;
use once_cell::sync::Lazy;

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tracing::warn;

use crate::common::collections::BTreeMap;
use crate::common::config::SpaceSwitchMethod;
use crate::layout_engine::Direction;
use crate::sys::dispatch::DispatchExt;
use crate::sys::screen::SpaceId;
use crate::sys::skylight::{
    CGEventPost, CGEventTapLocation, CGSGetActiveSpace, SLSMainConnectionID,
};

const K_CGS_EVENT_TYPE_FIELD: CGEventField = CGEventField(55);
const K_CGS_EVENT_MARKER: i64 = 29;
const K_CGS_EVENT_DOCK_CONTROL: i64 = 30;

const K_GESTURE_HID_TYPE_FIELD: CGEventField = CGEventField(110);
const K_GESTURE_SCROLL_Y_FIELD: CGEventField = CGEventField(119);
const K_GESTURE_SWIPE_MASK_FIELD: CGEventField = CGEventField(115);
const K_GESTURE_SWIPE_MOTION_FIELD: CGEventField = CGEventField(123);
const K_GESTURE_SWIPE_PROGRESS_FIELD: CGEventField = CGEventField(124);
const K_GESTURE_SWIPE_POSITION_X_FIELD: CGEventField = CGEventField(125);
const K_GESTURE_SWIPE_POSITION_Y_FIELD: CGEventField = CGEventField(126);
const K_GESTURE_SWIPE_VELOCITY_X_FIELD: CGEventField = CGEventField(129);
const K_GESTURE_SWIPE_VELOCITY_Y_FIELD: CGEventField = CGEventField(130);
const K_GESTURE_PHASE_FIELD: CGEventField = CGEventField(132);
const K_GESTURE_PHASE_MIRROR_FIELD: CGEventField = CGEventField(134);
const K_GESTURE_PROGRESS_BITS_FIELD: CGEventField = CGEventField(135);
const K_GESTURE_FLAVOR_FIELD: CGEventField = CGEventField(138);
const K_GESTURE_POSITION_FALLBACK_FIELD: CGEventField = CGEventField(139);
const K_GESTURE_TIMESTAMP_FIELD: CGEventField = CGEventField(169);

const K_IOHID_EVENT_TYPE_DOCK_SWIPE: i64 = 23;
const K_IOHID_EVENT_TYPE_VELOCITY: u32 = 9;
const K_IOHID_EVENT_TYPE_FLUID_TOUCH_GESTURE: u32 = 23;

const K_CG_GESTURE_MOTION_HORIZONTAL: i64 = 1;

const K_GESTURE_BEGAN: i64 = 1;
const K_GESTURE_CHANGED: i64 = 2;
const K_GESTURE_ENDED: i64 = 4;

/// Smallest positive subnormal f32. See `switch_space_legacy`.
const K_LEGACY_TRUE_MIN: f32 = f32::from_bits(1);
const K_LEGACY_SWITCH_VELOCITY: f64 = 400.0;

const K_EPSILON: f64 = 1e-15;
const K_INSTANT_SWITCH_VELOCITY: f64 = 100.0;
const K_GESTURE_FLAVOR_DOCK_PRIMARY: f64 = 3.0;
const K_GESTURE_SWIPE_POSITION_X: f64 = 0.1;
const K_GESTURE_DELAY_NS: i64 = 15 * 1_000_000;
const K_CGEVENT_DATA_HID_FIELD: u16 = 4205;
const K_CGEVENT_DATA_VERSION: i32 = 2;
const K_FIXED_16_16_SCALE: f64 = 65536.0;
const K_IOHID_EVENT_PHASE_SHIFT: u32 = 24;

static IS_MACOS_27_OR_NEWER: Lazy<bool> = Lazy::new(|| {
    let version = NSProcessInfo::processInfo().operatingSystemVersion();
    version.majorVersion >= 27
});

#[derive(Clone)]
enum CGEventDataElement {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Blob(Vec<u8>),
}

impl CGEventDataElement {
    fn deserialize(tag: u16, element_size: u16, data: &mut &[u8]) -> Option<Self> {
        match tag {
            0 => {
                if element_size == 1 {
                    Some(Self::I64(read_be_i64(data)?))
                } else {
                    Some(Self::Blob(read_exact(data, element_size as usize)?.to_vec()))
                }
            }
            1 if element_size == 1 => Some(Self::I32(read_be_i32(data)?)),
            3 => match element_size {
                1 => Some(Self::F32(read_be_f32(data)?)),
                2 => Some(Self::F64(read_be_f64(data)?)),
                _ => None,
            },
            _ => None,
        }
    }

    fn serialize(self, field: u16, out: &mut Vec<u8>) -> Option<()> {
        match self {
            Self::I32(value) => {
                write_field_header(out, 1, 0b01, field);
                out.extend_from_slice(&value.to_be_bytes());
            }
            Self::I64(value) => {
                write_field_header(out, 1, 0b00, field);
                out.extend_from_slice(&value.to_be_bytes());
            }
            Self::F32(value) => {
                write_field_header(out, 1, 0b11, field);
                out.extend_from_slice(&value.to_bits().to_be_bytes());
            }
            Self::F64(value) => {
                write_field_header(out, 2, 0b11, field);
                out.extend_from_slice(&value.to_bits().to_be_bytes());
            }
            Self::Blob(value) => {
                let element_size = u16::try_from(value.len()).ok()?;
                write_field_header(out, element_size, 0b00, field);
                out.extend_from_slice(&value);
            }
        }

        Some(())
    }
}

struct CGEventData {
    version: i32,
    fields: BTreeMap<u16, CGEventDataElement>,
}

#[repr(C, packed)]
struct IOHIDSystemQueueElement {
    timestamp: u64,
    sender_id: u64,
    options: u32,
    attribute_length: u32,
    event_count: u32,
}

#[repr(C, packed)]
struct IOHIDEventBase {
    size: u32,
    event_type: u32,
    options: u32,
    depth: u8,
    reserved: [u8; 3],
}

#[repr(C, packed)]
struct IOHIDFluidTouchGestureData {
    base: IOHIDEventBase,
    position_x: i32,
    position_y: i32,
    position_z: i32,
    swipe_mask: u32,
    gesture_motion: u16,
    gesture_flavor: u16,
    swipe_progress: i32,
}

#[repr(C, packed)]
struct IOHIDVelocityEventData {
    base: IOHIDEventBase,
    velocity_x: i32,
    velocity_y: i32,
    velocity_z: i32,
}

/// Gap between chained switches. One switch is a begin/end pair spaced
/// `K_GESTURE_DELAY_NS` apart, and the Dock needs to settle before it will
/// accept the next one, so steps are spaced well clear of that pair.
const K_SPACE_STEP_DELAY_NS: i64 = 90 * 1_000_000;

/// Switches to a macOS space by its position on the active display, 1-based to
/// match the way macOS numbers desktops.
///
/// yabai had `space --focus N`; rift's own workspaces are a separate concept
/// that hides windows offscreen, so this is the command for people who want
/// their native spaces and keep rift for tiling within them. Nothing happens if
/// the index is out of range or already active.
pub unsafe fn switch_to_space_index(index: usize, method: SpaceSwitchMethod) {
    let Some(steps) = steps_to_space_index(index) else {
        return;
    };
    if let Some(target) = space_at_index(index)
        && teleport_to_space(target, method)
    {
        return;
    }
    let direction = if steps > 0 {
        Direction::Right
    } else {
        Direction::Left
    };
    unsafe { switch_space_repeated(direction, steps.unsigned_abs()) };
}

/// How long the scripting addition gets to land a switch before the gesture
/// takes over. A healthy call measures in tenths of a millisecond, so this is a
/// wide margin rather than a tight budget.
const TELEPORT_DEADLINE: Duration = Duration::from_millis(40);
const TELEPORT_POLL: Duration = Duration::from_millis(2);
/// Consecutive misses before the addition is set aside, in `Auto` only.
const TELEPORT_MISS_LIMIT: u32 = 3;
const TELEPORT_COOLDOWN: Duration = Duration::from_secs(30);

static TELEPORT_MISSES: AtomicU32 = AtomicU32::new(0);
static TELEPORT_COOLDOWN_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Switches by asking the scripting addition, reporting whether the space
/// actually changed.
///
/// False means nothing happened and the caller should fall back: the addition
/// either refused the command or did not land it, so a fallback cannot switch
/// twice. The check is what makes `Auto` safe — the alternative, trusting the
/// call and gesturing as well, would double-switch whenever it did work.
fn teleport_to_space(target: SpaceId, method: SpaceSwitchMethod) -> bool {
    if method == SpaceSwitchMethod::Gesture {
        return false;
    }
    if active_space() == target {
        return true;
    }
    if method == SpaceSwitchMethod::Auto && !teleport_is_available() {
        return false;
    }
    if !crate::sys::scripting_addition::focus_space(target.get()) {
        note_teleport_miss(method);
        return false;
    }

    // The addition sets the window server's current space directly, but the
    // read can trail the write, so judge on the deadline rather than one
    // sample.
    let started = Instant::now();
    while started.elapsed() < TELEPORT_DEADLINE {
        if active_space() == target {
            TELEPORT_MISSES.store(0, Ordering::Relaxed);
            return true;
        }
        std::thread::sleep(TELEPORT_POLL);
    }

    warn!(
        ?target,
        "Scripting addition did not land the switch; using the gesture"
    );
    note_teleport_miss(method);
    false
}

fn teleport_is_available() -> bool {
    match *TELEPORT_COOLDOWN_UNTIL.lock() {
        Some(until) => Instant::now() >= until,
        None => true,
    }
}

fn note_teleport_miss(method: SpaceSwitchMethod) {
    if method != SpaceSwitchMethod::Auto {
        return;
    }
    let misses = TELEPORT_MISSES.fetch_add(1, Ordering::Relaxed) + 1;
    if misses >= TELEPORT_MISS_LIMIT {
        warn!(
            "Scripting addition missed {misses} switches; using the gesture for {}s",
            TELEPORT_COOLDOWN.as_secs()
        );
        TELEPORT_MISSES.store(0, Ordering::Relaxed);
        *TELEPORT_COOLDOWN_UNTIL.lock() = Some(Instant::now() + TELEPORT_COOLDOWN);
    }
}

/// The space one step from the active one, or `None` at either end.
fn space_in_direction(direction: Direction) -> Option<SpaceId> {
    let step: isize = match direction {
        Direction::Right => 1,
        Direction::Left => -1,
        _ => return None,
    };
    let spaces = spaces_on_active_display()?;
    let current = spaces.iter().position(|id| *id == active_space())?;
    spaces.get(current.checked_add_signed(step)?).copied()
}

/// The macOS space at 1-based `index` on the display holding the active space.
pub fn space_at_index(index: usize) -> Option<SpaceId> {
    let target = index.checked_sub(1)?;
    spaces_on_active_display()?.get(target).copied()
}

/// The currently active macOS space.
pub fn active_space() -> SpaceId {
    SpaceId::new(unsafe { CGSGetActiveSpace(SLSMainConnectionID()) })
}

/// The ordered spaces of the display holding the active space.
fn spaces_on_active_display() -> Option<Vec<SpaceId>> {
    let active = active_space();
    crate::sys::screen::managed_display_space_ids()
        .into_values()
        .find(|ids| ids.contains(&active))
}

/// Signed number of spaces between the active one and `index` on the same
/// display. `None` when the index is out of range or the space is already
/// active.
fn steps_to_space_index(index: usize) -> Option<isize> {
    let target = index.checked_sub(1)?;
    let active = active_space();

    let spaces = spaces_on_active_display()?;
    if target >= spaces.len() {
        return None;
    }

    let current = spaces.iter().position(|id| *id == active)?;
    let steps = target as isize - current as isize;
    (steps != 0).then_some(steps)
}

/// Posts `steps` switches in one direction, one after another.
///
/// The Dock drives each switch itself, so they have to be spaced out in time
/// rather than posted back to back.
unsafe fn switch_space_repeated(direction: Direction, steps: usize) {
    if steps == 0 {
        return;
    }
    unsafe { switch_space(direction, SpaceSwitchMethod::Gesture) };
    if steps > 1 {
        queue::main().after_f_s(
            Time::new_after(Time::NOW, K_SPACE_STEP_DELAY_NS),
            (direction, steps - 1),
            |(direction, remaining)| unsafe { switch_space_repeated(direction, remaining) },
        );
    }
}

pub unsafe fn switch_space(direction: Direction, method: SpaceSwitchMethod) {
    if let Some(target) = space_in_direction(direction)
        && teleport_to_space(target, method)
    {
        return;
    }
    if *IS_MACOS_27_OR_NEWER {
        unsafe { switch_space_macos_27(direction) };
    } else {
        unsafe { switch_space_legacy(direction) };
    }
}

/// Pre-macOS-27 path, following joshuarli/iss (ISC).
///
/// The previous implementation here posted a two-phase gesture to the HID tap
/// with a progress magnitude of +/-2.25, and does not move the space at all on
/// macOS 26. What works is iss's shape, which differs in every part that turns
/// out to matter: three phases rather than two, posted to the *session* tap,
/// each paired with a plain gesture companion event, and — the load-bearing
/// detail — a progress field holding the bits of the smallest positive
/// subnormal float rather than a real magnitude. iss's own note on that value
/// is "empirically, +/-FLT_TRUE_MIN here makes switching instant".
unsafe fn switch_space_legacy(direction: Direction) {
    let Some(right) = horizontal_direction_value(direction, false, true) else {
        return;
    };

    // Begin and Changed carry no velocity; the terminal event's velocity is what
    // makes the WindowServer treat the gesture as a flick and skip the slide.
    post_legacy_pair(K_GESTURE_BEGAN, right, None);
    post_legacy_pair(K_GESTURE_CHANGED, right, None);
    post_legacy_pair(
        K_GESTURE_ENDED,
        right,
        Some(if right {
            K_LEGACY_SWITCH_VELOCITY
        } else {
            -K_LEGACY_SWITCH_VELOCITY
        }),
    );
}

/// One phase of a legacy dock swipe: the DockControl event and the companion
/// gesture event the Dock expects alongside it.
fn post_legacy_pair(phase: i64, right: bool, velocity_x: Option<f64>) {
    let dock = new_event();
    configure_dock_swipe_event(&dock, phase);

    // Field 135 holds the raw bits of an f32, not a number the field's own type
    // would suggest. Only its sign and non-zeroness appear to matter.
    let progress = if right {
        K_LEGACY_TRUE_MIN
    } else {
        -K_LEGACY_TRUE_MIN
    };
    set_integer_fields(
        &dock,
        &[(K_GESTURE_PROGRESS_BITS_FIELD, progress.to_bits() as i32 as i64)],
    );
    set_double_fields(
        &dock,
        &[
            (K_GESTURE_SCROLL_Y_FIELD, 0.0),
            (K_GESTURE_POSITION_FALLBACK_FIELD, K_LEGACY_TRUE_MIN as f64),
        ],
    );
    if let Some(velocity_x) = velocity_x {
        set_double_fields(
            &dock,
            &[
                (K_GESTURE_SWIPE_VELOCITY_X_FIELD, velocity_x),
                (K_GESTURE_SWIPE_VELOCITY_Y_FIELD, 0.0),
            ],
        );
    }

    let companion = new_event();
    set_integer_fields(&companion, &[(K_CGS_EVENT_TYPE_FIELD, K_CGS_EVENT_MARKER)]);

    post_events(CGEventTapLocation::Session, [&dock, &companion]);
}

unsafe fn switch_space_macos_27(direction: Direction) {
    let Some(gesture_sign) = horizontal_direction_value(direction, 1.0, -1.0) else {
        return;
    };

    let begin_progress = K_EPSILON * gesture_sign;
    let end_velocity = K_INSTANT_SWITCH_VELOCITY * gesture_sign;

    let begin_event = dock_control_gesture_event(direction, K_GESTURE_BEGAN, begin_progress, None);
    post_augmented_session_event(&begin_event);

    queue::main().after_f_s(
        Time::new_after(Time::NOW, K_GESTURE_DELAY_NS),
        (direction, begin_progress, end_velocity),
        |(direction, end_progress, end_velocity)| {
            let end_event = dock_control_gesture_event(
                direction,
                K_GESTURE_ENDED,
                end_progress,
                Some(end_velocity),
            );
            post_augmented_session_event(&end_event);
        },
    );
}

fn dock_control_gesture_event(
    direction: Direction,
    phase: i64,
    progress: f64,
    velocity_x: Option<f64>,
) -> CFRetained<CGEvent> {
    let event = new_event();
    configure_dock_swipe_event(&event, phase);
    set_integer_fields(
        &event,
        &[(
            K_GESTURE_SWIPE_MASK_FIELD,
            swipe_mask_for_direction(direction) as i64,
        )],
    );
    set_double_fields(&event, &[(K_GESTURE_SWIPE_PROGRESS_FIELD, progress)]);

    if let Some(velocity_x) = velocity_x {
        set_double_fields(&event, &[(K_GESTURE_SWIPE_VELOCITY_X_FIELD, velocity_x)]);
    }

    set_double_fields(
        &event,
        &[
            (K_GESTURE_FLAVOR_FIELD, K_GESTURE_FLAVOR_DOCK_PRIMARY),
            (K_GESTURE_TIMESTAMP_FIELD, unsafe { mach_absolute_time() as f64 }),
            (K_GESTURE_SWIPE_POSITION_X_FIELD, K_GESTURE_SWIPE_POSITION_X),
        ],
    );
    event
}

fn post_augmented_session_event(event: &CGEvent) {
    let posted = augment_event_with_hid_payload(event).or_else(|| CGEvent::new_copy(Some(event)));
    if let Some(posted) = posted {
        post_events(CGEventTapLocation::Session, [&posted]);
    }
}

fn augment_event_with_hid_payload(event: &CGEvent) -> Option<CFRetained<CGEvent>> {
    let serialized = CGEvent::new_data(None, Some(event))?;
    let event_data = deserialize_cgevent_data(unsafe { serialized.as_bytes_unchecked() })?;
    let mut fields = event_data.fields;
    fields.insert(
        K_CGEVENT_DATA_HID_FIELD,
        CGEventDataElement::Blob(generate_iohid_system_queue_element(event)),
    );

    let serialized = serialize_cgevent_data(CGEventData {
        version: event_data.version,
        fields,
    })?;
    let data = CFData::from_bytes(&serialized);
    CGEvent::from_data(None, Some(&data))
}

fn deserialize_cgevent_data(mut data: &[u8]) -> Option<CGEventData> {
    let version = read_be_i32(&mut data)?;
    if version != K_CGEVENT_DATA_VERSION {
        return None;
    }

    let mut fields = BTreeMap::new();
    while !data.is_empty() {
        let element_size = read_be_u16(&mut data)?;
        let tag_and_field = read_be_u16(&mut data)?;
        let tag = (tag_and_field >> 14) & 0x0003;
        let field = tag_and_field & 0x3FFF;
        fields.insert(
            field,
            CGEventDataElement::deserialize(tag, element_size, &mut data)?,
        );
    }

    Some(CGEventData { version, fields })
}

fn serialize_cgevent_data(event_data: CGEventData) -> Option<Vec<u8>> {
    if event_data.version != K_CGEVENT_DATA_VERSION {
        return None;
    }

    let mut out = Vec::new();
    out.extend_from_slice(&event_data.version.to_be_bytes());

    for (field, element) in event_data.fields {
        element.serialize(field, &mut out)?;
    }

    Some(out)
}

fn write_field_header(out: &mut Vec<u8>, element_size: u16, tag: u16, field: u16) {
    out.extend_from_slice(&element_size.to_be_bytes());
    out.extend_from_slice(&(((tag & 0x0003) << 14) | (field & 0x3FFF)).to_be_bytes());
}

fn generate_iohid_system_queue_element(event: &CGEvent) -> Vec<u8> {
    let phase = CGEvent::integer_value_field(Some(event), K_GESTURE_PHASE_FIELD);
    let motion = CGEvent::integer_value_field(Some(event), K_GESTURE_SWIPE_MOTION_FIELD) as u16;
    let progress = CGEvent::double_value_field(Some(event), K_GESTURE_SWIPE_PROGRESS_FIELD);
    let position_x = CGEvent::double_value_field(Some(event), K_GESTURE_SWIPE_POSITION_X_FIELD);
    let position_y = CGEvent::double_value_field(Some(event), K_GESTURE_SWIPE_POSITION_Y_FIELD);
    let velocity_x = CGEvent::double_value_field(Some(event), K_GESTURE_SWIPE_VELOCITY_X_FIELD);
    let velocity_y = CGEvent::double_value_field(Some(event), K_GESTURE_SWIPE_VELOCITY_Y_FIELD);
    let swipe_mask = CGEvent::integer_value_field(Some(event), K_GESTURE_SWIPE_MASK_FIELD) as u32;

    let has_velocity = velocity_x != 0.0 || velocity_y != 0.0 || phase == K_GESTURE_ENDED;
    let event_count = if has_velocity { 2 } else { 1 };

    let header = IOHIDSystemQueueElement {
        timestamp: cg_event_timestamp_or_now(event),
        sender_id: 0,
        options: 0,
        attribute_length: 0,
        event_count,
    };
    let gesture = IOHIDFluidTouchGestureData {
        base: IOHIDEventBase {
            size: size_of::<IOHIDFluidTouchGestureData>() as u32,
            event_type: K_IOHID_EVENT_TYPE_FLUID_TOUCH_GESTURE,
            options: ((phase as u32) & 0xFF) << K_IOHID_EVENT_PHASE_SHIFT,
            depth: 0,
            reserved: [0; 3],
        },
        position_x: double_to_fixed_16_16(position_x),
        position_y: double_to_fixed_16_16(position_y),
        position_z: 0,
        swipe_mask,
        gesture_motion: motion,
        gesture_flavor: K_GESTURE_FLAVOR_DOCK_PRIMARY as u16,
        swipe_progress: double_to_fixed_16_16(progress),
    };

    let mut out = Vec::with_capacity(
        size_of::<IOHIDSystemQueueElement>()
            + size_of::<IOHIDFluidTouchGestureData>()
            + if has_velocity {
                size_of::<IOHIDVelocityEventData>()
            } else {
                0
            },
    );
    extend_packed(&mut out, &header);
    extend_packed(&mut out, &gesture);

    if has_velocity {
        let velocity = IOHIDVelocityEventData {
            base: IOHIDEventBase {
                size: size_of::<IOHIDVelocityEventData>() as u32,
                event_type: K_IOHID_EVENT_TYPE_VELOCITY,
                options: 0,
                depth: 1,
                reserved: [0; 3],
            },
            velocity_x: double_to_fixed_16_16(velocity_x),
            velocity_y: double_to_fixed_16_16(velocity_y),
            velocity_z: 0,
        };
        extend_packed(&mut out, &velocity);
    }

    out
}

fn new_event() -> CFRetained<CGEvent> {
    CGEvent::new(None).expect("CGEventCreate should succeed")
}

fn configure_dock_swipe_event(event: &CGEvent, phase: i64) {
    set_integer_fields(
        event,
        &[
            (K_CGS_EVENT_TYPE_FIELD, K_CGS_EVENT_DOCK_CONTROL),
            (K_GESTURE_HID_TYPE_FIELD, K_IOHID_EVENT_TYPE_DOCK_SWIPE),
            (K_GESTURE_PHASE_FIELD, phase),
            (K_GESTURE_PHASE_MIRROR_FIELD, phase),
            (K_GESTURE_SWIPE_MOTION_FIELD, K_CG_GESTURE_MOTION_HORIZONTAL),
        ],
    );
}

fn set_integer_fields(event: &CGEvent, fields: &[(CGEventField, i64)]) {
    for &(field, value) in fields {
        CGEvent::set_integer_value_field(Some(event), field, value);
    }
}

fn set_double_fields(event: &CGEvent, fields: &[(CGEventField, f64)]) {
    for &(field, value) in fields {
        CGEvent::set_double_value_field(Some(event), field, value);
    }
}

fn post_events<'a>(
    location: CGEventTapLocation,
    events: impl IntoIterator<Item = &'a CFRetained<CGEvent>>,
) {
    for event in events {
        unsafe { CGEventPost(location, CFRetained::as_ptr(event).as_ptr().cast()) };
    }
}

fn horizontal_direction_value<T>(direction: Direction, left: T, right: T) -> Option<T> {
    match direction {
        Direction::Left => Some(left),
        Direction::Right => Some(right),
        _ => None,
    }
}

fn swipe_mask_for_direction(direction: Direction) -> u32 {
    match direction {
        Direction::Left => 8,
        Direction::Right => 4,
        Direction::Up => 2,
        Direction::Down => 1,
    }
}

fn cg_event_timestamp_or_now(event: &CGEvent) -> u64 {
    let timestamp = CGEvent::timestamp(Some(event));
    if timestamp == 0 {
        unsafe { mach_absolute_time() }
    } else {
        timestamp
    }
}

fn double_to_fixed_16_16(value: f64) -> i32 {
    let fixed = (value * K_FIXED_16_16_SCALE) as i32;
    if fixed == 0 && value != 0.0 {
        if value.is_sign_negative() { -1 } else { 1 }
    } else {
        fixed
    }
}

fn extend_packed<T>(bytes: &mut Vec<u8>, value: &T) {
    let ptr = std::ptr::from_ref(value).cast::<u8>();
    let slice = unsafe { std::slice::from_raw_parts(ptr, size_of::<T>()) };
    bytes.extend_from_slice(slice);
}

fn read_exact<'a>(data: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if data.len() < len {
        return None;
    }
    let (head, tail) = data.split_at(len);
    *data = tail;
    Some(head)
}

fn read_be_u16(data: &mut &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(read_exact(data, 2)?.try_into().ok()?))
}

fn read_be_i32(data: &mut &[u8]) -> Option<i32> {
    Some(i32::from_be_bytes(read_exact(data, 4)?.try_into().ok()?))
}

fn read_be_i64(data: &mut &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(read_exact(data, 8)?.try_into().ok()?))
}

fn read_be_f32(data: &mut &[u8]) -> Option<f32> {
    Some(f32::from_bits(u32::from_be_bytes(
        read_exact(data, 4)?.try_into().ok()?,
    )))
}

fn read_be_f64(data: &mut &[u8]) -> Option<f64> {
    Some(f64::from_bits(u64::from_be_bytes(
        read_exact(data, 8)?.try_into().ok()?,
    )))
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

#[cfg(test)]
mod tests {
    use super::{
        IOHIDFluidTouchGestureData, IOHIDSystemQueueElement, IOHIDVelocityEventData,
        double_to_fixed_16_16,
    };

    #[test]
    fn fixed_point_keeps_non_zero_epsilon() {
        assert_eq!(double_to_fixed_16_16(0.0), 0);
        assert_eq!(double_to_fixed_16_16(f64::MIN_POSITIVE), 1);
        assert_eq!(double_to_fixed_16_16(-f64::MIN_POSITIVE), -1);
    }

    #[test]
    fn hid_layout_sizes_match_expected() {
        assert_eq!(std::mem::size_of::<IOHIDSystemQueueElement>(), 28);
        assert_eq!(std::mem::size_of::<IOHIDFluidTouchGestureData>(), 40);
        assert_eq!(std::mem::size_of::<IOHIDVelocityEventData>(), 28);
    }
}
