// TDD tests for Emacs-style line-editing keys in InputState.
//
// RED phase: these tests define the expected Emacs behavior before implementation.
//
// Emacs / readline bindings being implemented:
//   C-a   beginning of line        C-e   end of line
//   C-b   backward char            C-f   forward char
//   C-d   delete char forward      C-h   delete char backward (backspace)
//   C-k   kill to end of line      C-u   kill to start of line
//   C-w   kill word backward       M-d   kill word forward
//   M-Backspace  kill word backward
//   M-b   backward word            M-f   forward word
//   C-y   yank (paste last kill from the kill ring)
//   C-p   previous history         C-n   next history
//   M-<   beginning of history     M->   end of history

use gladiator_tui::state::InputState;

// --- Word movement (M-f / M-b) ----------------------------------------------

#[test]
fn emacs_forward_word_basic() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward();
    assert_eq!(input.cursor(), 5); // end of "hello"
}

#[test]
fn emacs_forward_word_skips_whitespace() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // -> 5
    input.cursor_word_forward(); // -> 11 (end of "world")
    assert_eq!(input.cursor(), 11);
}

#[test]
fn emacs_forward_word_from_middle() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    // cursor at 11 -> move back to 1 (inside "hello")
    for _ in 0..10 {
        input.cursor_left();
    }
    assert_eq!(input.cursor(), 1);
    input.cursor_word_forward(); // -> 5 (end of "hello")
    assert_eq!(input.cursor(), 5);
}

#[test]
fn emacs_forward_word_punctuation() {
    let mut input = InputState::new();
    input.insert_str("foo.bar baz");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // "foo" is word -> 3
    assert_eq!(input.cursor(), 3);
    input.cursor_word_forward(); // skip ".bar": '.' non-word, then "bar" -> 7
    assert_eq!(input.cursor(), 7);
}

#[test]
fn emacs_backward_word_basic() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    // cursor at 11
    input.cursor_word_backward();
    assert_eq!(input.cursor(), 6); // start of "world"
}

#[test]
fn emacs_backward_word_twice() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    input.cursor_word_backward(); // -> 6
    input.cursor_word_backward(); // -> 0
    assert_eq!(input.cursor(), 0);
}

#[test]
fn emacs_backward_word_into_whitespace() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    // cursor at 6 (start of "world")
    for _ in 0..5 {
        input.cursor_left();
    }
    assert_eq!(input.cursor(), 6);
    input.cursor_word_backward(); // skip space, then "hello" -> 0
    assert_eq!(input.cursor(), 0);
}

// --- C-d: delete char forward ----------------------------------------------

#[test]
fn emacs_delete_char_forward() {
    let mut input = InputState::new();
    input.insert_str("abc");
    input.cursor_left(); // cursor at 2 (before 'c')
    input.delete_char_forward(); // delete 'c'
    assert_eq!(input.buffer(), "ab");
    assert_eq!(input.cursor(), 2);
}

#[test]
fn emacs_delete_char_forward_at_end() {
    let mut input = InputState::new();
    input.insert_str("abc");
    // cursor at 3 (end)
    input.delete_char_forward(); // nothing to delete
    assert_eq!(input.buffer(), "abc");
    assert_eq!(input.cursor(), 3);
}

#[test]
fn emacs_delete_char_forward_unicode() {
    let mut input = InputState::new();
    input.insert_str("héllo");
    // "héllo" bytes: h(0) é(1-2) l(3) l(4) o(5), len 6. cursor at end (6).
    input.cursor_left(); // 5
    input.cursor_left(); // 4
    input.cursor_left(); // 3 (before first 'l')
    input.delete_char_forward(); // remove 'l' at pos 3
    assert_eq!(input.buffer(), "hélo");
    assert_eq!(input.cursor(), 3);
}

// --- C-k: kill to end of line ----------------------------------------------

#[test]
fn emacs_kill_to_end_of_line() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // cursor at 5
    input.kill_to_end_of_line();
    assert_eq!(input.buffer(), "hello");
    assert_eq!(input.cursor(), 5);
    assert_eq!(input.kill_ring().last().unwrap(), " world");
}

#[test]
fn emacs_kill_to_end_of_line_nothing_after() {
    let mut input = InputState::new();
    input.insert_str("hello");
    input.kill_to_end_of_line(); // cursor at 5, nothing after
    assert_eq!(input.buffer(), "hello");
    assert_eq!(input.cursor(), 5);
    assert!(input.kill_ring().is_empty());
}

#[test]
fn emacs_kill_to_end_of_line_stops_at_newline() {
    let mut input = InputState::new();
    input.insert_str("ab\ncd");
    // cursor at 5 -> move to 1
    for _ in 0..4 {
        input.cursor_left();
    }
    assert_eq!(input.cursor(), 1);
    input.kill_to_end_of_line(); // kills "b", stops before '\n'
    assert_eq!(input.buffer(), "a\ncd");
    assert_eq!(input.cursor(), 1);
    assert_eq!(input.kill_ring().last().unwrap(), "b");
}

#[test]
fn emacs_kill_to_end_of_line_multiline() {
    let mut input = InputState::new();
    input.insert_str("line1\nline2");
    // cursor at 11 -> move to start of line2 (6)
    input.cursor_line_start();
    assert_eq!(input.cursor(), 6);
    input.kill_to_end_of_line(); // kills "line2"
    assert_eq!(input.buffer(), "line1\n");
    assert_eq!(input.cursor(), 6);
    assert_eq!(input.kill_ring().last().unwrap(), "line2");
}

// --- C-u: kill to start of line --------------------------------------------

#[test]
fn emacs_kill_to_start_of_line() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // cursor at 5
    input.kill_to_start_of_line(); // kill "hello"
    assert_eq!(input.buffer(), " world");
    assert_eq!(input.cursor(), 0);
    assert_eq!(input.kill_ring().last().unwrap(), "hello");
}

#[test]
fn emacs_kill_to_start_of_line_multiline() {
    let mut input = InputState::new();
    input.insert_str("line1\nline2");
    // cursor at 11 -> move to 9 (inside "line2", after "lin")
    for _ in 0..2 {
        input.cursor_left();
    }
    assert_eq!(input.cursor(), 9);
    input.kill_to_start_of_line(); // line_start=6, kill "lin"
    assert_eq!(input.buffer(), "line1\ne2");
    assert_eq!(input.cursor(), 6);
    assert_eq!(input.kill_ring().last().unwrap(), "lin");
}

#[test]
fn emacs_kill_to_start_of_line_at_line_start() {
    let mut input = InputState::new();
    input.insert_str("hello");
    input.cursor_line_start(); // cursor 0
    input.kill_to_start_of_line(); // nothing to kill
    assert_eq!(input.buffer(), "hello");
    assert_eq!(input.cursor(), 0);
    assert!(input.kill_ring().is_empty());
}

// --- M-d: kill word forward -------------------------------------------------

#[test]
fn emacs_kill_word_forward() {
    let mut input = InputState::new();
    input.insert_str("hello world foo");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // cursor at 5
    input.kill_word_forward(); // kill " world"
    assert_eq!(input.buffer(), "hello foo");
    assert_eq!(input.cursor(), 5);
    assert_eq!(input.kill_ring().last().unwrap(), " world");
}

#[test]
fn emacs_kill_word_forward_at_end() {
    let mut input = InputState::new();
    input.insert_str("hello");
    input.kill_word_forward(); // cursor 5, no word forward
    assert_eq!(input.buffer(), "hello");
    assert_eq!(input.cursor(), 5);
    assert!(input.kill_ring().is_empty());
}

// --- M-Backspace / C-w: kill word backward ----------------------------------

#[test]
fn emacs_kill_word_backward() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    // cursor at 11
    input.kill_word_backward(); // kill "world"
    assert_eq!(input.buffer(), "hello ");
    assert_eq!(input.cursor(), 6);
    assert_eq!(input.kill_ring().last().unwrap(), "world");
}

#[test]
fn emacs_kill_word_backward_across_whitespace() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    // cursor at 6 (start of "world")
    for _ in 0..5 {
        input.cursor_left();
    }
    assert_eq!(input.cursor(), 6);
    input.kill_word_backward(); // kill from 0 to 6 = "hello "
    assert_eq!(input.buffer(), "world");
    assert_eq!(input.cursor(), 0);
    assert_eq!(input.kill_ring().last().unwrap(), "hello ");
}

#[test]
fn emacs_kill_word_backward_at_start() {
    let mut input = InputState::new();
    input.insert_str("hello");
    input.kill_word_backward(); // cursor 5, word_backward_target=0, kill "hello"
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
    assert_eq!(input.kill_ring().last().unwrap(), "hello");
}

// --- C-y: yank from kill ring -----------------------------------------------

#[test]
fn emacs_yank_after_kill_to_end() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // 5
    input.kill_to_end_of_line(); // kill " world"
    assert_eq!(input.buffer(), "hello");
    input.yank(); // paste " world" at cursor
    assert_eq!(input.buffer(), "hello world");
    assert_eq!(input.cursor(), 11);
}

#[test]
fn emacs_yank_empty_kill_ring() {
    let mut input = InputState::new();
    input.insert_str("hello");
    input.yank(); // nothing to yank
    assert_eq!(input.buffer(), "hello");
    assert_eq!(input.cursor(), 5);
}

#[test]
fn emacs_yank_most_recent_kill() {
    let mut input = InputState::new();
    input.insert_str("abc def ghi");
    // cursor at 11 -> move to 7 (space before "ghi")
    for _ in 0..4 {
        input.cursor_left();
    }
    assert_eq!(input.cursor(), 7);
    input.kill_to_end_of_line(); // kill " ghi"
    // now buffer is "abc def", cursor at 7 -> move to 3 (space before "def")
    for _ in 0..4 {
        input.cursor_left();
    }
    assert_eq!(input.cursor(), 3);
    input.kill_to_end_of_line(); // kill " def"
    assert_eq!(input.buffer(), "abc");
    // yank brings back the most recent kill: " def"
    input.yank();
    assert_eq!(input.buffer(), "abc def");
}

// --- Kill ring: consecutive kills concatenate (emacs semantics) -------------

#[test]
fn emacs_consecutive_kills_concatenate_forward() {
    let mut input = InputState::new();
    input.insert_str("hello world foo");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // 5
    input.kill_word_forward(); // " world"
    input.kill_word_forward(); // " foo" — appended to previous kill
    assert_eq!(input.buffer(), "hello");
    assert_eq!(input.kill_ring().len(), 1);
    assert_eq!(input.kill_ring()[0], " world foo");
}

#[test]
fn emacs_consecutive_kills_concatenate_backward() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    // cursor at 11. Two backward kills should prepend.
    input.kill_word_backward(); // "world" (kill ring ["world"])
    // cursor at 6, buffer "hello "
    input.kill_word_backward(); // "hello " prepended -> "hello world"
    assert_eq!(input.buffer(), "");
    assert_eq!(input.kill_ring().len(), 1);
    assert_eq!(input.kill_ring()[0], "hello world");
}

#[test]
fn emacs_yank_after_consecutive_kills() {
    let mut input = InputState::new();
    input.insert_str("hello world foo");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // 5
    input.kill_word_forward(); // " world"
    input.kill_word_forward(); // " foo"
    assert_eq!(input.buffer(), "hello");
    input.yank(); // " world foo"
    assert_eq!(input.buffer(), "hello world foo");
}

#[test]
fn emacs_non_kill_breaks_concatenation() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    input.cursor_line_start(); // cursor at 0
    input.cursor_word_forward(); // 5
    input.kill_word_forward(); // " world" -> kill ring [" world"], buffer "hello"
    input.cursor_left(); // non-kill command breaks the chain, cursor 5 -> 4
    input.kill_word_forward(); // "o" -> new entry
    assert_eq!(input.buffer(), "hell");
    assert_eq!(input.kill_ring().len(), 2);
    assert_eq!(input.kill_ring()[0], " world");
    assert_eq!(input.kill_ring()[1], "o");
}

// --- History: M-< / M-> -----------------------------------------------------

#[test]
fn emacs_history_beginning() {
    let mut input = InputState::new();
    input.insert_str("first");
    let _ = input.submit();
    input.insert_str("second");
    let _ = input.submit();
    input.insert_str("third");
    let _ = input.submit();
    input.history_beginning();
    assert_eq!(input.buffer(), "first");
    assert_eq!(input.cursor(), 5);
}

#[test]
fn emacs_history_end() {
    let mut input = InputState::new();
    input.insert_str("first");
    let _ = input.submit();
    input.insert_str("second");
    let _ = input.submit();
    input.history_prev(); // "second"
    input.history_end();
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
}

#[test]
fn emacs_history_beginning_empty() {
    let mut input = InputState::new();
    input.history_beginning(); // no history
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
}
