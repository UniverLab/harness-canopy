//! Brian's Brain cellular automaton, ported from the web's `brain.ts`.
//!
//! 3-state automaton on a fine sub-grid (2×4 cells per character cell):
//! On → Dying → Off → On. Rule: an Off cell turns On if exactly 2 neighbors
//! are On. Toroidal wrapping so patterns flow across edges.
//!
//! The field is a machine at rest: it starts almost empty and the cursor is
//! the real input, seeding small propagating clusters along its path. Two much
//! quieter sources keep the last embers alive — a rate-limited revival, and,
//! once the cursor has been still long enough, a single cluster every few
//! seconds. The step rate is fixed at 130 ms; moving the pointer never speeds
//! the clock (the web's `brain.ts` is the reference for every constant here).

use std::time::{Duration, Instant};

/// Fixed step interval — never affected by pointer motion.
const STEP_MS: u64 = 130;
/// Fraction of sub-cells firing at init (web: `Math.random() < 0.003`).
const START_DENSITY: f64 = 0.003;
/// Pointer seed rate limit, in ms (web: `SEED_EVERY`).
const SEED_EVERY_MS: u64 = 200;
/// Character cells interpolated from the last seeded cell, at most
/// (web: `MAX_TRAIL`).
const MAX_TRAIL: i64 = 14;
/// Stillness before the field goes ultra-sparse (web: `IDLE_AFTER`).
const IDLE_AFTER_MS: u64 = 8000;
/// Cadence of the lone idle clusters (web: `IDLE_EVERY`).
const IDLE_EVERY_MS: u64 = 3000;
/// Revival batch rate limit (web: `REVIVE_EVERY`).
const REVIVE_EVERY_MS: u64 = 900;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CellState {
    Off,
    On,
    Dying,
}

/// Brian's Brain automaton. The struct stores the *character* (Braille) cell
/// dimensions in `rows`/`cols`; the automaton runs on the derived sub-grid
/// `gw × gh` (`cols*2 × rows*4`).
pub struct BriansBrain {
    /// Braille rows (character cells).
    pub rows: usize,
    /// Braille columns (character cells).
    pub cols: usize,
    /// Sub-grid width  = `cols * 2`.
    pub gw: usize,
    /// Sub-grid height = `rows * 4`.
    pub gh: usize,
    /// Sub-grid state, row-major, length `gw * gh`.
    pub grid: Vec<CellState>,
    last_step: Instant,
    /// When the pointer last seeded (rate limit).
    last_seed: Instant,
    /// Last seeded character cell, `-1` when no trail is being walked.
    trail_last_col: i32,
    trail_last_row: i32,
    /// Last pointer move, for the idle window. `None` until the first seed.
    last_move: Option<Instant>,
    /// Next lone idle cluster; `None` while not idle.
    idle_next_seed: Option<Instant>,
    /// Last revival batch, rate-limited by [`REVIVE_EVERY_MS`].
    last_revive: Option<Instant>,
}

impl BriansBrain {
    pub fn new(rows: usize, cols: usize) -> Self {
        let gw = cols * 2;
        let gh = rows * 4;
        let mut grid = vec![CellState::Off; gw * gh];
        // Start almost empty — isolated cells blink once and are gone; the
        // pointer wakes the field up.
        for cell in grid.iter_mut() {
            if rand::random::<f64>() < START_DENSITY {
                *cell = CellState::On;
            }
        }
        let now = Instant::now();
        Self {
            rows,
            cols,
            gw,
            gh,
            grid,
            last_step: now - Duration::from_millis(STEP_MS),
            last_seed: now - Duration::from_millis(SEED_EVERY_MS),
            trail_last_col: -1,
            trail_last_row: -1,
            last_move: None,
            idle_next_seed: None,
            last_revive: None,
        }
    }

    pub fn braille_rows(&self) -> usize {
        self.rows
    }

    pub fn braille_cols(&self) -> usize {
        self.cols
    }

    /// Toroidal index into the sub-grid.
    fn idx(&self, x: usize, y: usize) -> usize {
        (y % self.gh) * self.gw + (x % self.gw)
    }

    fn set_on(&mut self, x: usize, y: usize) {
        let i = self.idx(x, y);
        self.grid[i] = CellState::On;
    }

    /// A cluster of adjacent firing cells: neighbours then see exactly 2 firing
    /// cells and ignite, so reseeding propagates instead of dying out. Mirrors
    /// the web's `seedAt` — 3 cells, plus 2 extras each with probability 0.5.
    fn seed_at(&mut self, sx: usize, sy: usize) {
        self.set_on(sx, sy);
        self.set_on(sx + 1, sy);
        self.set_on(sx, sy + 1);
        if rand::random::<f64>() < 0.5 {
            self.set_on(sx + 1, sy + 1);
        }
        if rand::random::<f64>() < 0.5 {
            self.set_on(sx, sy.wrapping_sub(1));
        }
    }

    /// Plant the cluster under one character cell (a 2×4 sub-cell block).
    fn seed_char(&mut self, bx: usize, by: usize) {
        self.seed_at(bx * 2, by * 4);
    }

    fn seed_cluster(&mut self) {
        let x = rand::random::<u32>() as usize % self.gw.max(1);
        let y = rand::random::<u32>() as usize % self.gh.max(1);
        self.seed_at(x, y);
    }

    /// Forget the walked trail; the next pointer seed plants a fresh cluster
    /// instead of drawing a line across the field.
    pub fn reset_trail(&mut self) {
        self.trail_last_col = -1;
        self.trail_last_row = -1;
    }

    /// Seed the field from a pointer event at character cell `(bx, by)`. Rate
    /// limited to one seed per [`SEED_EVERY_MS`]; walks the path from the last
    /// seeded cell so the trail is a continuous line.
    pub fn seed_pointer(&mut self, bx: usize, by: usize) {
        let now = Instant::now();
        if now.duration_since(self.last_seed) < Duration::from_millis(SEED_EVERY_MS) {
            return;
        }
        self.last_seed = now;
        self.last_move = Some(now);
        self.idle_next_seed = None;

        let bx = bx.min(self.cols.saturating_sub(1));
        let by = by.min(self.rows.saturating_sub(1));

        if self.trail_last_col < 0 {
            self.seed_char(bx, by);
        } else {
            let dx = bx as f64 - self.trail_last_col as f64;
            let dy = by as f64 - self.trail_last_row as f64;
            let steps = (dx.hypot(dy).ceil() as i64).clamp(1, MAX_TRAIL);
            for i in 1..=steps {
                let s = i as f64 / steps as f64;
                let cx = (self.trail_last_col as f64 + dx * s).round() as i64;
                let cy = (self.trail_last_row as f64 + dy * s).round() as i64;
                self.seed_char(cx.max(0) as usize, cy.max(0) as usize);
            }
        }
        self.trail_last_col = bx as i32;
        self.trail_last_row = by as i32;
    }

    /// Advance the automaton one fixed step if [`STEP_MS`] has elapsed.
    /// Returns `true` when the field actually stepped. Pointer motion never
    /// changes the rate — this is the whole point of the rebrand.
    pub fn step(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_step) < Duration::from_millis(STEP_MS) {
            return false;
        }
        self.last_step = now;

        let total = self.gw * self.gh;
        let mut next = vec![CellState::Off; total];
        let mut firing = 0usize;
        for y in 0..self.gh {
            for x in 0..self.gw {
                let i = y * self.gw + x;
                next[i] = match self.grid[i] {
                    CellState::On => CellState::Dying,
                    CellState::Dying => CellState::Off,
                    CellState::Off => {
                        if self.count_on_neighbors(x, y) == 2 {
                            firing += 1;
                            CellState::On
                        } else {
                            CellState::Off
                        }
                    }
                };
            }
        }
        self.grid = next;

        // The cursor seeded the field above; these two only keep the machine
        // from going completely dark, and both are deliberately near-subliminal.
        let idle = self
            .last_move
            .is_some_and(|last| now.duration_since(last) > Duration::from_millis(IDLE_AFTER_MS));
        if idle {
            match self.idle_next_seed {
                None => self.idle_next_seed = Some(now + Duration::from_millis(IDLE_EVERY_MS)),
                Some(deadline) if now >= deadline => {
                    self.seed_cluster();
                    self.idle_next_seed = Some(now + Duration::from_millis(IDLE_EVERY_MS));
                }
                Some(_) => {}
            }
        } else {
            self.idle_next_seed = None;
            // Revival: only once activity has collapsed, and only rarely.
            // Integer arithmetic stands in for the web's floor(len * 0.0035)
            // and floor(len * 0.0004).
            let threshold = (total * 35 / 10_000).max(5);
            let due = self.last_revive.is_none_or(|last| {
                now.duration_since(last) > Duration::from_millis(REVIVE_EVERY_MS)
            });
            if firing < threshold && due {
                self.last_revive = Some(now);
                let clusters = (total * 4 / 10_000).max(1);
                for _ in 0..clusters {
                    self.seed_cluster();
                }
            }
        }
        true
    }

    fn count_on_neighbors(&self, x: usize, y: usize) -> usize {
        let mut count = 0;
        for dy in [-1i64, 0, 1] {
            for dx in [-1i64, 0, 1] {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let nx = (x as i64 + dx).rem_euclid(self.gw as i64) as usize;
                let ny = (y as i64 + dy).rem_euclid(self.gh as i64) as usize;
                if self.grid[self.idx(nx, ny)] == CellState::On {
                    count += 1;
                }
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn off_brain(rows: usize, cols: usize) -> BriansBrain {
        let mut brain = BriansBrain::new(rows, cols);
        for cell in brain.grid.iter_mut() {
            *cell = CellState::Off;
        }
        brain
    }

    fn count_on(brain: &BriansBrain) -> usize {
        brain
            .grid
            .iter()
            .filter(|cell| **cell == CellState::On)
            .count()
    }

    /// The step rate is fixed: seeding the field through the pointer must not
    /// change how many times the automaton advances over the same window.
    #[test]
    fn step_count_same_with_and_without_mouse() {
        let mut still = BriansBrain::new(8, 12);
        let mut moved = BriansBrain::new(8, 12);

        let start = Instant::now();
        let mut still_steps = 0usize;
        let mut moved_steps = 0usize;
        let mut last_seed = start;
        while start.elapsed() < Duration::from_secs(2) {
            if still.step() {
                still_steps += 1;
            }
            if moved.step() {
                moved_steps += 1;
            }
            if last_seed.elapsed() >= Duration::from_millis(50) {
                moved.seed_pointer(
                    (rand::random::<u32>() as usize) % moved.cols,
                    (rand::random::<u32>() as usize) % moved.rows,
                );
                last_seed = Instant::now();
            }
        }

        assert!(
            still_steps > 0,
            "the automaton must step at least once in 2s"
        );
        assert_eq!(
            still_steps, moved_steps,
            "pointer seeding must not alter the step count"
        );
    }

    #[test]
    fn pointer_seeds_firing_cells_under_the_cursor() {
        let mut brain = off_brain(6, 6);
        brain.seed_pointer(2, 1);
        assert!(count_on(&brain) > 0, "a pointer seed must light sub-cells");
        // The cluster is planted at the character cell's sub-origin.
        assert_eq!(brain.grid[brain.idx(4, 4)], CellState::On);
        assert_eq!(brain.grid[brain.idx(5, 4)], CellState::On);
        assert_eq!(brain.grid[brain.idx(4, 5)], CellState::On);
    }

    #[test]
    fn rate_limit_two_events_100ms_apart() {
        let mut brain = off_brain(6, 6);
        brain.seed_pointer(1, 1);
        let after_first = count_on(&brain);
        assert!(after_first > 0);

        std::thread::sleep(Duration::from_millis(100));
        brain.seed_pointer(1, 1);
        assert_eq!(
            count_on(&brain),
            after_first,
            "a second seed within 200ms must be a no-op"
        );
    }

    #[test]
    fn reset_trail_starts_a_fresh_cluster() {
        let mut brain = off_brain(6, 6);
        brain.seed_pointer(1, 1);
        brain.reset_trail();
        assert_eq!(brain.trail_last_col, -1);
        assert_eq!(brain.trail_last_row, -1);
    }
}
