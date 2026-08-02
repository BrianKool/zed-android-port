//! Keyboard input translation: Android `KeyEvent` (and the raw-fields
//! variant used by the ExtraWindowActivity JNI bridge) → gpui
//! `PlatformInput::KeyDown` / `KeyUp` / `ModifiersChanged`.
//!
//! Hardware keys only. IME / soft-keyboard composition lives in `ime.rs`
//! when that lands (see `deferred-soft-keyboard.md`).

use android_activity::input::{KeyAction, KeyEvent, Keycode, MetaState};
use gpui::{
    Capslock, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent, PlatformInput,
};

/// Convert an Android `KeyEvent` into a gpui `PlatformInput`.
///
/// Returns `None` when the event isn't translatable (e.g. `KeyAction::Multiple`,
/// which is reserved for synthesized character sequences from soft keyboards we
/// don't currently support).
pub(crate) fn translate_key_event(event: &KeyEvent) -> Option<PlatformInput> {
    let action = event.action();
    let keycode = event.key_code();
    let meta_state = event.meta_state();
    let modifiers = modifiers_from_meta(meta_state);

    if is_modifier_key(keycode) {
        return Some(PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers,
            capslock: capslock_from_meta(meta_state),
        }));
    }

    let keystroke = build_keystroke(keycode, modifiers, capslock_from_meta(meta_state));

    match action {
        KeyAction::Down => Some(PlatformInput::KeyDown(KeyDownEvent {
            keystroke,
            is_held: event.repeat_count() > 0,
            prefer_character_input: false,
        })),
        KeyAction::Up => Some(PlatformInput::KeyUp(KeyUpEvent { keystroke })),
        _ => None,
    }
}

/// Same shape as [`translate_key_event`] but takes raw Android KeyEvent
/// fields instead of an `android_activity::KeyEvent` object. Used by the
/// ExtraWindowActivity JNI bridge (`multi_window::nativeOnExtraKeyEvent`)
/// which sees `MotionEvent`/`KeyEvent` via Java reflection: those Java
/// objects can't be reconstructed into `android_activity::KeyEvent` on
/// the Rust side, so we accept the primitive fields and rebuild the
/// translation pipeline on top of them.
///
/// `action`: `KeyEvent.ACTION_DOWN` (0) or `ACTION_UP` (1).
/// `keycode_raw`: Android `KeyEvent.getKeyCode()` (AKEYCODE_*).
/// `meta_state_raw`: Android `KeyEvent.getMetaState()` (META_* bitfield).
/// `repeat_count`: `KeyEvent.getRepeatCount()` for auto-repeat detection.
pub(crate) fn translate_extra_key_event(
    action: i32,
    keycode_raw: u32,
    meta_state_raw: u32,
    repeat_count: i32,
) -> Option<PlatformInput> {
    let meta = MetaState(meta_state_raw);
    let keycode = Keycode::from(keycode_raw);
    let modifiers = modifiers_from_meta(meta);

    if is_modifier_key(keycode) {
        return Some(PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers,
            capslock: capslock_from_meta(meta),
        }));
    }

    let keystroke = build_keystroke(keycode, modifiers, capslock_from_meta(meta));

    // Android KeyEvent.ACTION_DOWN = 0, ACTION_UP = 1, ACTION_MULTIPLE = 2.
    // Same translation policy as `translate_key_event`: only Down/Up
    // produce inputs; Multiple is reserved for synthesized soft-keyboard
    // char sequences we don't currently support.
    match action {
        0 => Some(PlatformInput::KeyDown(KeyDownEvent {
            keystroke,
            is_held: repeat_count > 0,
            prefer_character_input: false,
        })),
        1 => Some(PlatformInput::KeyUp(KeyUpEvent { keystroke })),
        _ => None,
    }
}

pub(crate) fn modifiers_from_meta(meta: MetaState) -> Modifiers {
    Modifiers {
        shift: meta.shift_on(),
        control: meta.ctrl_on(),
        alt: meta.alt_on(),
        platform: meta.meta_on(),
        function: meta.function_on(),
    }
}

fn capslock_from_meta(meta: MetaState) -> Capslock {
    Capslock {
        on: meta.caps_lock_on(),
    }
}

fn is_modifier_key(code: Keycode) -> bool {
    use Keycode::*;
    matches!(
        code,
        ShiftLeft | ShiftRight | AltLeft | AltRight | CtrlLeft | CtrlRight | MetaLeft | MetaRight
    )
}

fn build_keystroke(code: Keycode, mut modifiers: Modifiers, capslock: Capslock) -> Keystroke {
    let (key, key_char) = if let Some(named) = named_key(code) {
        // Space is the one named key where gpui still wants a printable
        // key_char so text-input paths can insert " ".
        let key_char = matches!(code, Keycode::Space).then(|| " ".to_string());
        (named.to_string(), key_char)
    } else if let Some(ch) = lowercased_key(code) {
        let key = ch.to_string();
        let shifted = if ch.is_ascii_alphabetic() {
            modifiers.shift ^ capslock.on
        } else {
            modifiers.shift
        };
        let typed = if shifted {
            apply_shift(ch)
        } else {
            ch
        };
        (key, Some(typed.to_string()))
    } else {
        (format!("{code:?}").to_lowercase(), None)
    };

    // Drop the shift modifier for non-alpha single-char keys: the shifted
    // value is already in `key_char` and bindings like `shift-1` should match
    // as `!`. Mirrors X11's `keystroke_from_xkb` behavior.
    if modifiers.shift
        && key.chars().count() == 1
        && key.chars().next().map_or(false, |c| {
            c.to_lowercase().to_string() == c.to_uppercase().to_string()
        })
    {
        modifiers.shift = false;
    }

    Keystroke {
        modifiers,
        key,
        key_char,
    }
}

fn named_key(code: Keycode) -> Option<&'static str> {
    use Keycode::*;
    Some(match code {
        Enter | NumpadEnter => "enter",
        Tab => "tab",
        Space => "space",
        Del => "backspace",
        ForwardDel => "delete",
        Escape => "escape",
        DpadUp => "up",
        DpadDown => "down",
        DpadLeft => "left",
        DpadRight => "right",
        MoveHome => "home",
        MoveEnd => "end",
        PageUp => "pageup",
        PageDown => "pagedown",
        Insert => "insert",
        F1 => "f1",
        F2 => "f2",
        F3 => "f3",
        F4 => "f4",
        F5 => "f5",
        F6 => "f6",
        F7 => "f7",
        F8 => "f8",
        F9 => "f9",
        F10 => "f10",
        F11 => "f11",
        F12 => "f12",
        _ => return None,
    })
}

fn lowercased_key(code: Keycode) -> Option<char> {
    use Keycode::*;
    Some(match code {
        A => 'a',
        B => 'b',
        C => 'c',
        D => 'd',
        E => 'e',
        F => 'f',
        G => 'g',
        H => 'h',
        I => 'i',
        J => 'j',
        K => 'k',
        L => 'l',
        M => 'm',
        N => 'n',
        O => 'o',
        P => 'p',
        Q => 'q',
        R => 'r',
        S => 's',
        T => 't',
        U => 'u',
        V => 'v',
        W => 'w',
        X => 'x',
        Y => 'y',
        Z => 'z',
        Keycode0 => '0',
        Keycode1 => '1',
        Keycode2 => '2',
        Keycode3 => '3',
        Keycode4 => '4',
        Keycode5 => '5',
        Keycode6 => '6',
        Keycode7 => '7',
        Keycode8 => '8',
        Keycode9 => '9',
        Period => '.',
        Comma => ',',
        Slash => '/',
        Backslash => '\\',
        Semicolon => ';',
        Apostrophe => '\'',
        Grave => '`',
        Minus => '-',
        Equals => '=',
        LeftBracket => '[',
        RightBracket => ']',
        _ => return None,
    })
}

fn apply_shift(ch: char) -> char {
    match ch {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        _ => ch.to_ascii_uppercase(),
    }
}
