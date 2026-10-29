use super::*;
use crate::domain::models::Cli;
use ratatui::style::Color;

fn spawn_cat_agent() -> InteractiveAgent {
    InteractiveAgent::spawn(
        Cli::new("cat"),
        env!("CARGO_MANIFEST_DIR"),
        80,
        24,
        None,
        None,
        Color::Reset,
        Some("scroll-test-agent"),
        None,
        &[],
        None,
        None,
        None,
        None,
    )
    .expect("spawn cat as a stand-in interactive child")
}

#[test]
fn outside_alternate_screen_scrolls_canopy_own_buffer() {
    let mut agent = spawn_cat_agent();
    assert!(!agent.in_alternate_screen());
    agent.scroll_offset = 5;

    scroll_terminal_like_agent(&mut agent, -1, 3);

    // Routed locally: scroll_offset moved, no PTY write was needed.
    assert_eq!(agent.scroll_offset, 2);
    agent.kill();
}

#[test]
fn inside_alternate_screen_defers_to_the_child_instead() {
    let mut agent = spawn_cat_agent();
    // Feed the alternate-screen DECSET directly into the vt100 parser,
    // as a real full-screen child's own output would (rather than
    // relying on pty echo, which `cat` may or may not perform).
    agent.vt.lock().expect("vt lock").process(b"\x1b[?1049h");
    assert!(agent.in_alternate_screen());
    agent.scroll_offset = 5;

    scroll_terminal_like_agent(&mut agent, -1, 3);

    // Routed to the child: local scroll_offset is untouched.
    assert_eq!(agent.scroll_offset, 5);
    agent.kill();
}
