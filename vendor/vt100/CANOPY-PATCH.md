# Canopy patch vs upstream vt100 0.16.2

Single behavioural difference in `src/grid.rs`, `Grid::scroll_up`:

- Upstream: a row scrolled off the screen is pushed to scrollback only when
  no scroll region is active
  (`if self.scrollback_len > 0 && !self.scroll_region_active()`).
- Patched: a row removed from the top of the screen is pushed to scrollback
  whenever the scroll region's top is row 0
  (`if self.scrollback_len > 0 && self.scroll_top == 0`), whether or not the
  region's bottom is the last row. A region whose top is not row 0 keeps
  upstream behaviour (nothing saved), matching xterm, which saves the lines
  that leave a top-anchored scroll region into scrollback.

The scrollback length cap and the `scrollback_offset` adjustment are
unchanged. The alternate screen is unaffected: it is built with
`scrollback_len = 0`, so nothing is ever saved there.
