use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Result of applying a key event to a text buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAction {
    /// The buffer was modified.
    Edited,
    /// User pressed Enter — caller should commit.
    Submit,
    /// User pressed Esc — caller should cancel.
    Cancel,
    /// Nothing happened.
    Ignored,
}

/// Apply a single key event to a text buffer, supporting the common terminal
/// line-edit shortcuts. Safe across platforms — when a modifier a particular
/// terminal can't forward (e.g. Cmd on macOS Terminal.app) the Ctrl fallback
/// still works.
///
/// Bindings:
/// - any printable char     → insert
/// - Backspace              → delete previous char
/// - Alt+Backspace or Ctrl+W → delete previous word
/// - Cmd+Backspace or Ctrl+U → clear buffer
/// - Enter                  → `Submit`
/// - Esc                    → `Cancel`
pub fn apply(buffer: &mut String, key: &KeyEvent) -> TextAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let cmd = key.modifiers.contains(KeyModifiers::SUPER);

    match key.code {
        KeyCode::Enter => TextAction::Submit,
        KeyCode::Esc => TextAction::Cancel,

        KeyCode::Backspace if alt || cmd && alt => {
            delete_word(buffer);
            TextAction::Edited
        }
        KeyCode::Backspace if cmd => {
            if buffer.is_empty() {
                TextAction::Ignored
            } else {
                buffer.clear();
                TextAction::Edited
            }
        }
        KeyCode::Backspace => {
            if buffer.pop().is_some() {
                TextAction::Edited
            } else {
                TextAction::Ignored
            }
        }

        KeyCode::Char('w') if ctrl => {
            delete_word(buffer);
            TextAction::Edited
        }
        KeyCode::Char('u') if ctrl => {
            if buffer.is_empty() {
                TextAction::Ignored
            } else {
                buffer.clear();
                TextAction::Edited
            }
        }

        KeyCode::Char(c) if !ctrl && !alt && !cmd => {
            buffer.push(c);
            TextAction::Edited
        }

        _ => TextAction::Ignored,
    }
}

/// Like `apply`, but only passes printable characters through `allow_char`.
/// Edit shortcuts (backspace, Ctrl-W, Ctrl-U, Alt-⌫, Cmd-⌫) always run — only
/// plain character insertion is filtered. Use this for numeric-only fields.
pub fn apply_filtered(
    buffer: &mut String,
    key: &KeyEvent,
    allow_char: impl Fn(char) -> bool,
) -> TextAction {
    if let KeyCode::Char(c) = key.code {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let cmd = key.modifiers.contains(KeyModifiers::SUPER);
        if !ctrl && !alt && !cmd && !allow_char(c) {
            return TextAction::Ignored;
        }
    }
    apply(buffer, key)
}

/// Remove any trailing word-boundary characters, then pop characters until we
/// hit another boundary or exhaust the buffer. Boundaries are whitespace plus
/// `-` and `_` so "foo-bar-baz" and "foo_bar_baz" behave like "foo bar baz".
fn delete_word(buffer: &mut String) {
    while buffer.chars().next_back().is_some_and(is_word_boundary) {
        buffer.pop();
    }
    while let Some(c) = buffer.chars().next_back() {
        if is_word_boundary(c) {
            break;
        }
        buffer.pop();
    }
}

fn is_word_boundary(c: char) -> bool {
    c.is_whitespace() || c == '-' || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn basic_typing_and_backspace() {
        let mut b = String::new();
        apply(&mut b, &key(KeyCode::Char('h'), KeyModifiers::NONE));
        apply(&mut b, &key(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(b, "hi");
        apply(&mut b, &key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(b, "h");
    }

    #[test]
    fn alt_backspace_deletes_word() {
        let mut b = String::from("one two three");
        apply(&mut b, &key(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(b, "one two ");
        apply(&mut b, &key(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(b, "one ");
    }

    #[test]
    fn ctrl_w_deletes_word() {
        let mut b = String::from("my cool file");
        apply(&mut b, &key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(b, "my cool ");
    }

    #[test]
    fn cmd_backspace_clears_buffer() {
        let mut b = String::from("something");
        apply(&mut b, &key(KeyCode::Backspace, KeyModifiers::SUPER));
        assert!(b.is_empty());
    }

    #[test]
    fn ctrl_u_clears_buffer() {
        let mut b = String::from("abc");
        apply(&mut b, &key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(b.is_empty());
    }

    #[test]
    fn delete_word_handles_trailing_space() {
        let mut b = String::from("foo bar   ");
        delete_word(&mut b);
        assert_eq!(b, "foo ");
    }

    #[test]
    fn delete_word_treats_dash_as_boundary() {
        let mut b = String::from("foo-bar-baz");
        delete_word(&mut b);
        assert_eq!(b, "foo-bar-");
        delete_word(&mut b);
        assert_eq!(b, "foo-");
        delete_word(&mut b);
        assert_eq!(b, "");
    }

    #[test]
    fn delete_word_treats_underscore_as_boundary() {
        let mut b = String::from("my_cool_trace");
        delete_word(&mut b);
        assert_eq!(b, "my_cool_");
        delete_word(&mut b);
        assert_eq!(b, "my_");
    }

    #[test]
    fn delete_word_mixed_separators() {
        let mut b = String::from("one_two three-four");
        delete_word(&mut b);
        assert_eq!(b, "one_two three-");
        delete_word(&mut b);
        assert_eq!(b, "one_two ");
    }

    #[test]
    fn enter_and_esc_signal_submit_cancel() {
        let mut b = String::from("x");
        assert_eq!(
            apply(&mut b, &key(KeyCode::Enter, KeyModifiers::NONE)),
            TextAction::Submit
        );
        assert_eq!(
            apply(&mut b, &key(KeyCode::Esc, KeyModifiers::NONE)),
            TextAction::Cancel
        );
    }
}
