//! PTY processes, terminal emulation, and incremental presentation frames.
pub mod pty;

use libghostty_vt::{
    Terminal, TerminalOptions,
    render::{CellIterator, RenderState, RowIterator},
    screen::{CellWide, TrackedGridRef},
    style::{Style, StyleColor, Underline},
    terminal::{Mode, Point, PointCoordinate, PointSpace},
};
use pty::Event;
use serde::{Deserialize, Serialize};
use std::{
    fmt::Write as _,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

const SCROLLBACK_ROWS: usize = 10_000;
// Despite the C/Rust options docs saying "lines", Ghostty's pinned Screen.init
// passes this value to PageList as a byte budget. Allow enough native storage
// for wide terminals; HistoryState separately enforces the 10,000-row limit.
const GHOSTTY_SCROLLBACK_BYTES: usize = 64 * 1024 * 1024;

type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Frame {
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub history: History,
    pub alternate: bool,
    pub full: String,
    pub delta: String,
    pub exited: bool,
    pub code: Option<i32>,
    pub error: Option<String>,
}
// Snapshots share immutable history; wire updates contain only new rows. This
// also lets skipped watch frames and reconnects recover without losing output.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct History {
    pub epoch: u64,
    pub start: u64,
    pub end: u64,
    pub reset: bool,
    pub rows: Arc<Vec<Arc<String>>>,
}
impl History {
    pub fn since(&self, cursor: &mut Option<(u64, u64)>) -> Self {
        let offset = cursor.and_then(|(epoch, end)| {
            (epoch == self.epoch && end >= self.start && end <= self.end)
                .then(|| (end - self.start) as usize)
        });
        let update = Self {
            reset: offset.is_none(),
            rows: offset.map_or_else(
                || self.rows.clone(),
                |offset| Arc::new(self.rows[offset..].to_vec()),
            ),
            ..self.clone()
        };
        *cursor = Some((self.epoch, self.end));
        update
    }
    pub fn restore(&self, previous: &Self) -> Option<Self> {
        if self.end < self.start || self.end - self.start > SCROLLBACK_ROWS as u64 {
            return None;
        }
        let rows = if self.reset {
            self.rows.clone()
        } else {
            if self.epoch != previous.epoch
                || self.start < previous.start
                || self.end != previous.end + self.rows.len() as u64
            {
                return None;
            }
            let drop = (self.start - previous.start) as usize;
            if drop > previous.rows.len() {
                return None;
            }
            Arc::new(
                previous.rows[drop..]
                    .iter()
                    .chain(self.rows.iter())
                    .cloned()
                    .collect(),
            )
        };
        (rows.len() as u64 == self.end - self.start).then(|| Self {
            rows,
            reset: true,
            ..self.clone()
        })
    }
}
#[derive(Default)]
struct HistoryState {
    snapshot: History,
    last: Option<TrackedGridRef>,
    dimensions: (u16, u16),
}
impl HistoryState {
    fn update(&mut self, terminal: &Terminal<'_, '_>) -> Result<History> {
        // The alternate screen has no history. Keep the primary cache intact
        // while applications such as vim or less own the terminal.
        if terminal.active_screen().map_err(|e| e.to_string())?
            == libghostty_vt::screen::Screen::Alternate
        {
            return Ok(self.snapshot.clone());
        }
        let count = terminal.scrollback_rows().map_err(|e| e.to_string())?;
        let dimensions = (
            terminal.cols().map_err(|e| e.to_string())?,
            terminal.rows().map_err(|e| e.to_string())?,
        );
        let anchor = self
            .last
            .as_ref()
            .map(|last| last.point(PointSpace::History))
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten();
        let base = count.saturating_sub(SCROLLBACK_ROWS);
        let keep = anchor.map(|point| (point.y as usize + 1).saturating_sub(base));
        let rebuild = self.dimensions != dimensions
            || (!self.snapshot.rows.is_empty() && keep.is_none())
            || keep.is_some_and(|keep| keep > self.snapshot.rows.len() || keep > count - base);
        let from = if rebuild {
            base
        } else {
            anchor.map_or(base, |point| (point.y as usize + 1).max(base))
        };
        if rebuild || from < count || keep.is_some_and(|keep| keep < self.snapshot.rows.len()) {
            let snapshot = &mut self.snapshot;
            if rebuild {
                snapshot.epoch += 1;
                snapshot.start = 0;
                snapshot.rows = Arc::default();
            }
            let rows = Arc::make_mut(&mut snapshot.rows);
            if !rebuild {
                let discard = rows.len().saturating_sub(keep.unwrap_or(0));
                rows.drain(..discard);
                snapshot.start += discard as u64;
            }
            let palette = terminal.color_palette().map_err(|e| e.to_string())?;
            for y in from..count {
                let mut text = String::from("\x1b[0m");
                let mut style = Style::default();
                let mut chars = vec!['\0'; 32];
                for x in 0..dimensions.0 {
                    let grid = terminal
                        .grid_ref(Point::History(PointCoordinate { x, y: y as u32 }))
                        .map_err(|e| e.to_string())?;
                    let width = grid
                        .cell()
                        .and_then(|cell| cell.wide())
                        .map_err(|e| e.to_string())?;
                    if matches!(width, CellWide::SpacerHead | CellWide::SpacerTail) {
                        continue;
                    }
                    let mut next = grid.style().map_err(|e| e.to_string())?;
                    for color in [
                        &mut next.fg_color,
                        &mut next.bg_color,
                        &mut next.underline_color,
                    ] {
                        if let StyleColor::Palette(index) = *color {
                            *color = StyleColor::Rgb(palette.get(index));
                        }
                    }
                    if next != style {
                        sgr(&mut text, next);
                        style = next;
                    }
                    let len = match grid.graphemes(&mut chars) {
                        Ok(len) => len,
                        Err(libghostty_vt::Error::OutOfSpace { required }) => {
                            chars.resize(required, '\0');
                            grid.graphemes(&mut chars).map_err(|e| e.to_string())?
                        }
                        Err(error) => return Err(error.to_string()),
                    };
                    if len == 0 {
                        text.push(' ');
                    } else {
                        text.extend(chars[..len].iter());
                    }
                }
                text.push_str("\x1b[0m");
                rows.push(Arc::new(text));
            }
            snapshot.end = snapshot.start + rows.len() as u64;
            snapshot.reset = true;
        }
        self.dimensions = dimensions;
        self.last = if count > 0 {
            Some(
                terminal
                    .track_grid_ref(Point::History(PointCoordinate {
                        x: 0,
                        y: count as u32 - 1,
                    }))
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        Ok(self.snapshot.clone())
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Input {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Binary { data: Vec<u8> },
}
pub struct Handle {
    pub input: mpsc::Sender<Input>,
    pub frames: watch::Receiver<Frame>,
    pub stopped: Arc<AtomicBool>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}
pub fn spawn(cwd: PathBuf) -> Result<Handle> {
    let (input, incoming) = mpsc::channel(64);
    let (frames, receiver) = watch::channel(Frame {
        cols: 80,
        rows: 24,
        ..Default::default()
    });
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    std::thread::Builder::new()
        .name("session-terminal".into())
        .spawn(move || {
            if let Err(error) = run(cwd, incoming, &frames, stop) {
                frames.send_modify(|frame| {
                    frame.seq += 1;
                    frame.exited = true;
                    frame.error = Some(error);
                    frame.delta.clear();
                });
            }
            // Stop acknowledgements are sent only after run's Shell has dropped
            // and its owned processes have been cleaned up and reaped.
            if !frames.borrow().exited {
                frames.send_modify(|frame| {
                    frame.seq += 1;
                    frame.exited = true;
                    frame.delta.clear();
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Handle {
        input,
        frames: receiver,
        stopped,
    })
}

fn run(
    cwd: PathBuf,
    mut incoming: mpsc::Receiver<Input>,
    frames: &watch::Sender<Frame>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    // Ghostty is created, used and dropped on this thread: no unsafe Send wrapper.
    let mut terminal = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: GHOSTTY_SCROLLBACK_BYTES,
    })
    .map_err(|e| e.to_string())?;
    terminal
        .set_default_fg_color(Some(libghostty_vt::style::RgbColor {
            r: 0xe2,
            g: 0xe8,
            b: 0xf0,
        }))
        .map_err(|e| e.to_string())?;
    terminal
        .set_default_bg_color(Some(libghostty_vt::style::RgbColor {
            r: 0x10,
            g: 0x12,
            b: 0x18,
        }))
        .map_err(|e| e.to_string())?;
    let replies = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let reply_sink = replies.clone();
    terminal
        .on_pty_write(move |_, bytes| reply_sink.borrow_mut().extend_from_slice(bytes))
        .map_err(|e| e.to_string())?;
    let (events, output) = std::sync::mpsc::sync_channel(64);
    let mut shell =
        pty::spawn(&pty::shell(&cwd), 80, 24, true, events).map_err(|e| e.to_string())?;
    let mut previous = Vec::new();
    let mut history_state = HistoryState::default();
    let mut dirty = true;
    let mut last_frame = Instant::now() - Duration::from_secs(1);
    let mut exit = None;
    let mut exit_at = None;
    let mut eof = false;
    let mut next = None;
    loop {
        if stop.load(Ordering::Acquire) {
            for event in next.take().into_iter().chain(output.try_iter().take(16)) {
                if let Event::Output(data) = event {
                    terminal.vt_write(&data);
                }
            }
            publish(
                &terminal,
                &mut history_state,
                &mut previous,
                frames,
                false,
                None,
            )?;
            return Ok(());
        }
        // Bound processing per tick, preserving complete input chunks and resize order.
        for _ in 0..64 {
            match incoming.try_recv() {
                Ok(Input::Input { data }) => shell.write(data.into_bytes())?,
                Ok(Input::Binary { data }) => shell.write(data)?,
                Ok(Input::Resize { cols, rows }) => {
                    let (cols, rows) = (cols.clamp(20, 300), rows.clamp(5, 120));
                    if terminal.cols().ok() != Some(cols) || terminal.rows().ok() != Some(rows) {
                        shell.resize(cols, rows)?;
                        terminal
                            .resize(cols, rows, 0, 0)
                            .map_err(|e| e.to_string())?;
                        previous.clear();
                        dirty = true;
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        // Drain at most 16 chunks (256 KiB) per tick so chatty programs cannot starve input.
        for _ in 0..16 {
            let Some(event) = next.take().or_else(|| output.try_recv().ok()) else {
                break;
            };
            match event {
                Event::Output(data) => {
                    terminal.vt_write(&data);
                    dirty = true;
                }
                Event::Closed => {
                    eof = true;
                    break;
                }
                Event::Failed(error) => return Err(error),
            }
        }
        // Responses queue behind earlier browser input, never competing with it.
        shell.write(std::mem::take(&mut *replies.borrow_mut()))?;
        if exit_at.is_none() && shell.exited().map_err(|e| e.to_string())? {
            exit_at = Some(Instant::now());
        }
        // Wait briefly for final output: ConPTY can report exit before its last
        // output, and a backgrounded job can hold a Unix PTY open.
        let done = exit_at.is_some()
            && (eof || exit_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1)));
        if done {
            exit = shell.finish().map_err(|e| e.to_string())?;
        }
        if (dirty && last_frame.elapsed() >= Duration::from_millis(33)) || done {
            publish(
                &terminal,
                &mut history_state,
                &mut previous,
                frames,
                done,
                exit,
            )?;
            dirty = false;
            last_frame = Instant::now();
        }
        if done {
            return Ok(());
        }
        if eof {
            std::thread::sleep(Duration::from_millis(10));
        } else {
            match output.recv_timeout(Duration::from_millis(10)) {
                Ok(event) => next = Some(event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => eof = true,
            }
        }
    }
}

fn publish(
    terminal: &Terminal<'_, '_>,
    history_state: &mut HistoryState,
    previous: &mut Vec<String>,
    frames: &watch::Sender<Frame>,
    exited: bool,
    code: Option<i32>,
) -> Result<()> {
    let history = history_state.update(terminal)?;
    let alternate = terminal
        .active_screen()
        .map_err(|error| error.to_string())?
        == libghostty_vt::screen::Screen::Alternate;
    let (full, delta, rows) = display(terminal, previous)?;
    *previous = rows;
    frames.send_modify(|frame| {
        frame.seq += 1;
        frame.cols = terminal.cols().unwrap_or(80);
        frame.rows = terminal.rows().unwrap_or(24);
        frame.history = history;
        frame.alternate = alternate;
        frame.full = full;
        frame.delta = delta;
        frame.exited = exited;
        frame.code = code;
    });
    Ok(())
}

fn sgr(out: &mut String, style: Style) {
    out.push_str("\x1b[0");
    for (flag, code) in [
        (style.bold, 1),
        (style.faint, 2),
        (style.italic, 3),
        (style.blink, 5),
        (style.inverse, 7),
        (style.invisible, 8),
        (style.strikethrough, 9),
        (style.overline, 53),
    ] {
        if flag {
            let _ = write!(out, ";{code}");
        }
    }
    if style.underline != Underline::None {
        let _ = write!(out, ";4:{}", style.underline as u32);
    }
    for (color, code) in [
        (style.fg_color, 38),
        (style.bg_color, 48),
        (style.underline_color, 58),
    ] {
        match color {
            StyleColor::None => {}
            StyleColor::Palette(index) => {
                let _ = write!(out, ";{code};5;{}", index.0);
            }
            StyleColor::Rgb(rgb) => {
                let _ = write!(out, ";{code};2;{};{};{}", rgb.r, rgb.g, rgb.b);
            }
        }
    }
    out.push('m');
}
fn display(
    terminal: &Terminal<'_, '_>,
    previous: &[String],
) -> Result<(String, String, Vec<String>)> {
    let mut state = RenderState::new().map_err(|e| e.to_string())?;
    let snapshot = state.update(terminal).map_err(|e| e.to_string())?;
    let mut row_iterator = RowIterator::new().map_err(|e| e.to_string())?;
    let mut cell_iterator = CellIterator::new().map_err(|e| e.to_string())?;
    let mut iterator = row_iterator.update(&snapshot).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    while let Some(row) = iterator.next() {
        let mut text = String::from("\x1b[0m");
        let mut style = Style::default();
        let mut cells = cell_iterator.update(row).map_err(|e| e.to_string())?;
        while let Some(cell) = cells.next() {
            let width = cell
                .raw_cell()
                .and_then(|c| c.wide())
                .map_err(|e| e.to_string())?;
            if matches!(width, CellWide::SpacerHead | CellWide::SpacerTail) {
                continue;
            }
            let mut next = cell.style().map_err(|e| e.to_string())?;
            // Resolve palette colors on the host; palette updates then redraw
            // affected rows without relying on the browser's initial palette.
            if let Some(color) = cell.fg_color().map_err(|e| e.to_string())? {
                next.fg_color = StyleColor::Rgb(color);
            }
            if let Some(color) = cell.bg_color().map_err(|e| e.to_string())? {
                next.bg_color = StyleColor::Rgb(color);
            }
            if next != style {
                sgr(&mut text, next);
                style = next;
            }
            let chars = cell.graphemes().map_err(|e| e.to_string())?;
            if chars.is_empty() {
                text.push(' ');
            } else {
                for c in chars {
                    text.push(c);
                }
            }
        }
        rows.push(text);
    }
    // xterm is a presentation surface: no origin mode, scrolling regions or host
    // queries reach it. Ghostty retains all emulator internals across attachments.
    let prefix = "\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[?6l\x1b[r";
    let mut full = format!("{prefix}\x1b[0m\x1b[2J");
    let mut delta = prefix.to_string();
    let colors = snapshot.colors().map_err(|e| e.to_string())?;
    let mut theme = String::new();
    for (number, color) in [(10, colors.foreground), (11, colors.background)] {
        let _ = write!(
            theme,
            "\x1b]{number};rgb:{:02x}/{:02x}/{:02x}\x1b\\",
            color.r, color.g, color.b
        );
    }
    full.push_str(&theme);
    delta.push_str(&theme);
    for (index, row) in rows.iter().enumerate() {
        let line = format!("\x1b[{};1H\x1b[0m\x1b[2K{row}", index + 1);
        full.push_str(&line);
        if previous.get(index) != Some(row) {
            delta.push_str(&line);
        }
    }
    let mut suffix = format!(
        "\x1b[0m\x1b[{};{}H\x1b[?25{}",
        terminal.cursor_y().map_err(|e| e.to_string())? + 1,
        terminal.cursor_x().map_err(|e| e.to_string())? + 1,
        if terminal.is_cursor_visible().unwrap_or(true) {
            'h'
        } else {
            'l'
        }
    );
    for (mode, number) in [
        (Mode::BRACKETED_PASTE, 2004),
        (Mode::DECCKM, 1),
        (Mode::KEYPAD_KEYS, 66),
        (Mode::X10_MOUSE, 9),
        (Mode::NORMAL_MOUSE, 1000),
        (Mode::BUTTON_MOUSE, 1002),
        (Mode::ANY_MOUSE, 1003),
        (Mode::FOCUS_EVENT, 1004),
        (Mode::UTF8_MOUSE, 1005),
        (Mode::SGR_MOUSE, 1006),
        (Mode::URXVT_MOUSE, 1015),
    ] {
        let _ = write!(
            suffix,
            "\x1b[?{number}{}",
            if terminal.mode(mode).unwrap_or(false) {
                'h'
            } else {
                'l'
            }
        );
    }
    suffix.push_str("\x1b[?2026l");
    full.push_str(&suffix);
    delta.push_str(&suffix);
    Ok((full, delta, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn terminal() -> Terminal<'static, 'static> {
        Terminal::new(TerminalOptions {
            cols: 40,
            rows: 8,
            max_scrollback: 100,
        })
        .unwrap()
    }
    #[test]
    fn presentation_round_trips_wide_combining_colors_and_scroll_regions() {
        let mut host = terminal();
        host.vt_write("hello 界 e\u{301}\r\n\x1b[38;2;100;50;200mcolored\x1b[0m\x1b[3;6r\x1b[6;1Hbottom\nscroll\x1b[?2004h\x1b[?1006h\x1b[?1000h".as_bytes());
        let (full, _, rows) = display(&host, &[]).unwrap();
        let mut browser = terminal();
        browser.vt_write(full.as_bytes());
        assert_eq!(display(&browser, &[]).unwrap().2, rows);
        assert_eq!(browser.cursor_x().unwrap(), host.cursor_x().unwrap());
        assert_eq!(browser.cursor_y().unwrap(), host.cursor_y().unwrap());
        assert!(browser.mode(Mode::BRACKETED_PASTE).unwrap());
        host.vt_write(b"\x1b[4;1Hnew\x1b[?25l");
        let (_, delta, changed) = display(&host, &rows).unwrap();
        browser.vt_write(delta.as_bytes());
        assert_eq!(display(&browser, &[]).unwrap().2, changed);
        let mut fresh = terminal();
        fresh.vt_write(display(&host, &[]).unwrap().0.as_bytes());
        assert_eq!(display(&fresh, &[]).unwrap().2, changed);
        assert!(!fresh.is_cursor_visible().unwrap());
    }
    #[test]
    fn ghostty_answers_queries_without_a_browser() {
        let mut host = terminal();
        let replies = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = replies.clone();
        host.on_pty_write(move |_, data| sink.borrow_mut().extend_from_slice(data))
            .unwrap();
        host.vt_write(b"\x1b[3;5H\x1b[6n");
        assert_eq!(&*replies.borrow(), b"\x1b[3;5R");
        assert!(!display(&host, &[]).unwrap().0.contains("\x1b[6n"));
    }
    #[test]
    fn history_tracks_append_eviction_resize_clear_and_alternate_screen() {
        let mut host = terminal();
        let mut history = HistoryState::default();
        let mut cursor = None;
        let mut replica = History::default();
        for batch in 0..12 {
            for row in 0..25 {
                host.vt_write(
                    format!("\x1b[31mline-{} 界 e\u{301}\x1b[0m\r\n", batch * 25 + row).as_bytes(),
                );
            }
            let snapshot = history.update(&host).unwrap();
            let fresh = HistoryState::default().update(&host).unwrap();
            assert_eq!(snapshot.rows, fresh.rows, "batch {batch}");
            assert!(snapshot.rows.len() <= SCROLLBACK_ROWS);
            if batch % 3 == 0 {
                let patch = snapshot.since(&mut cursor);
                replica = patch.restore(&replica).expect("valid history patch");
                assert_eq!(replica.rows, snapshot.rows);
            }
        }
        let before = history.update(&host).unwrap();
        host.vt_write(b"\x1b[?1049halternate-only\r\n");
        assert_eq!(history.update(&host).unwrap().rows, before.rows);
        host.vt_write(b"\x1b[?1049l");
        host.resize(25, 10, 0, 0).unwrap();
        let resized = history.update(&host).unwrap();
        assert_ne!(resized.epoch, before.epoch);
        assert_eq!(
            resized.rows,
            HistoryState::default().update(&host).unwrap().rows
        );
        host.vt_write(b"\x1b[3J");
        let cleared = history.update(&host).unwrap();
        assert!(cleared.rows.is_empty());
        assert!(cleared.since(&mut cursor).reset);
    }
    #[test]
    fn scrollback_snapshot_is_bounded_to_ten_thousand_rows() {
        let mut host = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 5,
            max_scrollback: GHOSTTY_SCROLLBACK_BYTES,
        })
        .unwrap();
        let output = (0..10_100)
            .map(|row| format!("bounded-{row:05}\r\n"))
            .collect::<String>();
        host.vt_write(output.as_bytes());
        let mut state = HistoryState::default();
        let snapshot = state.update(&host).unwrap();
        assert_eq!(snapshot.rows.len(), SCROLLBACK_ROWS);
        assert!(!snapshot.rows[0].contains("bounded-00000"));
        assert!(snapshot.rows.last().unwrap().contains("bounded-10095"));
        let mut cursor = None;
        snapshot.since(&mut cursor);
        host.vt_write(b"bounded-next\r\n");
        let next = state.update(&host).unwrap();
        assert_eq!(next.rows.len(), SCROLLBACK_ROWS);
        assert_eq!(next.start, snapshot.start + 1);
        let patch = next.since(&mut cursor);
        assert!(!patch.reset);
        assert_eq!(patch.rows.len(), 1);
        assert_eq!(patch.restore(&snapshot).unwrap().rows, next.rows);
    }
    #[test]
    fn history_wire_updates_are_incremental_and_recover_skipped_history() {
        let mut host = terminal();
        let mut state = HistoryState::default();
        let mut cursor = None;
        host.vt_write(b"first\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\nseventh\r\neighth\r\nninth\r\n");
        let first = state.update(&host).unwrap();
        let patch = first.since(&mut cursor);
        assert!(patch.reset);
        assert!(!patch.rows.is_empty());
        let mut replica = patch.restore(&History::default()).unwrap();
        host.vt_write(b"tenth\r\n");
        let next = state.update(&host).unwrap();
        let patch = next.since(&mut cursor);
        assert!(!patch.reset);
        assert_eq!(patch.rows.len(), 1);
        replica = patch.restore(&replica).unwrap();
        assert_eq!(replica.rows, next.rows);
        assert!(next.since(&mut cursor).rows.is_empty());
        let mut invalid = patch.clone();
        invalid.end += 2;
        assert!(invalid.restore(&replica).is_none());
    }
    #[tokio::test]
    async fn shell_keeps_state_drains_final_output_and_reports_real_exit() {
        let handle = spawn(std::env::temp_dir()).unwrap();
        handle.input.send(Input::Input {data:"export AW_TERMINAL_TEST=retained\nprintf 'color:%s:%s\\n' \"$TERM\" \"${NO_COLOR-unset}\"\nprintf 'marker-%s\\n' \"$AW_TERMINAL_TEST\"\nexit 7\n".into()}).await.unwrap();
        let mut frames = handle.frames.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if frames.borrow().exited {
                    break;
                }
                frames.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let frame = frames.borrow().clone();
        assert_eq!(frame.code, Some(7));
        assert!(
            frame.full.contains("color:xterm-256color:unset"),
            "{}",
            frame.full
        );
        assert!(frame.full.contains("marker-retained"), "{}", frame.full);
    }
}
