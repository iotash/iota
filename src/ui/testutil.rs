//! The UI suites' shared harness (`cfg(test)` only): one open surface driven exactly as the loop
//! drives it ([`Surf`]), the key constructors, and a loop [`Model`] over the test-seam region
//! ([`test_model`]) with the model-level key helpers, and the headless terminal stack's two ends —
//! a shared byte sink ([`SharedBuf`]) and a scripted event source ([`ChannelEvents`]) — plus the spinner
//! glyphs ([`SPINNER_GLYPHS`]) that a "nothing is spinning" scan looks for. On unix, a test run
//! again on a real terminal ([`under_script`]) and how it tells the parent what happened.
#![allow(clippy::panic, clippy::expect_used)]

use std::io::{self, Write};
use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::sync::lock;
use crate::text::ansi::strip_sgr;
use crate::ui::facade::{Panel, TabbedResult};
use crate::ui::runtime::event_loop::{EventSource, LoopShared, Model};
use crate::ui::surface::tabbed::PanelState;
use crate::ui::surface::{SurfaceEffect, SurfaceState};

/// The spinner's ten glyphs as one string, for "no spinner on screen" scans.
pub(crate) const SPINNER_GLYPHS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

// --- the surface ------------------------------------------------------------

/// One open surface driven exactly as the loop drives it: keys through the pure key ladder, rows
/// through `render` at `width` columns (80 unless opened [`Surf::sized`]) — the surface block alone,
/// the frame around it is the loop's. `content()` RENDERS, which is what resolves a pending centre;
/// the view-search tests lean on that, so the two must not drift apart.
pub(crate) struct Surf {
    /// The state under test, reachable for the assertions that read it directly.
    pub(crate) st: SurfaceState,
    width: u16,
}

impl Surf {
    /// Opens `panels` at 80 columns, Enter committing everything.
    pub(crate) fn open(panels: Vec<Panel>) -> Self {
        Self::wizard(false, panels)
    }

    /// `enter_advances` = the ask-wizard shape.
    pub(crate) fn wizard(enter_advances: bool, panels: Vec<Panel>) -> Self {
        Self {
            st: SurfaceState::new(enter_advances, panels),
            width: 80,
        }
    }

    /// Opens `panels` at `width × height` — the window size a picker's preview pane is laid out for.
    pub(crate) fn sized(panels: Vec<Panel>, width: u16, height: u16) -> Self {
        let mut st = SurfaceState::new(false, panels);
        if height > 0 {
            st.set_room(height, height);
        }
        Self { st, width }
    }

    pub(crate) fn press(&mut self, k: KeyEvent) -> SurfaceEffect {
        self.st.key(k)
    }

    /// Presses a key that must leave the surface open.
    pub(crate) fn tap(&mut self, k: KeyEvent) {
        assert!(
            !matches!(self.press(k), SurfaceEffect::Close(_)),
            "key closed the surface unexpectedly"
        );
    }

    pub(crate) fn typed(&mut self, s: &str) {
        for c in s.chars() {
            self.tap(ch(c));
        }
    }

    /// The state of panel `i`.
    pub(crate) fn ps(&self, i: usize) -> &PanelState {
        &self.st.slots[i].state
    }

    pub(crate) fn rows(&mut self) -> Vec<String> {
        self.st.render(self.width).rows
    }

    pub(crate) fn content(&mut self) -> String {
        self.rows().join("\n")
    }

    pub(crate) fn plain(&mut self) -> String {
        strip_sgr(&self.content())
    }

    /// The trailing hint row (or the query field that replaces it).
    pub(crate) fn hint(&mut self) -> String {
        strip_sgr(&self.rows().pop().unwrap_or_default())
    }

    /// The loop's generation-guarded refresh pass.
    pub(crate) fn tick(&mut self) {
        self.st.tick();
    }

    /// `/needle` + Enter: into the walker.
    pub(crate) fn search(&mut self, q: &str) {
        self.tap(ch('/'));
        self.typed(q);
        self.tap(key(KeyCode::Enter));
    }

    /// The first panel's browser entries, by name.
    pub(crate) fn names(&self) -> Vec<String> {
        self.ps(0).entries.iter().map(|e| e.name.clone()).collect()
    }

    /// The rendered row carrying the switch knob, stripped and right-trimmed.
    pub(crate) fn toggle_row(&mut self) -> String {
        self.plain()
            .lines()
            .find(|l| l.contains('●'))
            .map(|l| l.trim_end().to_owned())
            .expect("no switch row rendered")
    }
}

pub(crate) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A text key as crossterm delivers it (uppercase carries SHIFT).
pub(crate) fn ch(c: char) -> KeyEvent {
    let m = if c.is_ascii_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    KeyEvent::new(KeyCode::Char(c), m)
}

pub(crate) fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// The result a closing effect carried.
pub(crate) fn closed(e: SurfaceEffect) -> TabbedResult {
    match e {
        SurfaceEffect::Close(r) => r,
        _ => panic!("expected the surface to close"),
    }
}

// --- the loop model -----------------------------------------------------------

/// A loop model over the test-seam region at 80×24.
pub(crate) fn test_model() -> Model {
    let width = Arc::new(AtomicU16::new(80));
    let height = Arc::new(AtomicU16::new(24));
    Model::new(LoopShared { width, height })
}

pub(crate) fn type_text(m: &mut Model, s: &str) {
    for c in s.chars() {
        m.handle_key(key(KeyCode::Char(c)));
    }
}

pub(crate) fn enter(m: &mut Model) {
    m.handle_key(key(KeyCode::Enter));
}

pub(crate) fn up(m: &mut Model) {
    m.handle_key(key(KeyCode::Up));
}

pub(crate) fn down(m: &mut Model) {
    m.handle_key(key(KeyCode::Down));
}

pub(crate) fn ctrl_c(m: &mut Model) {
    m.handle_key(ctrl('c'));
}

/// SGR-stripped frame rows.
pub(crate) fn plain(m: &mut Model) -> Vec<String> {
    m.frame_view().rows.iter().map(|r| strip_sgr(r)).collect()
}

// --- the headless terminal stack ---------------------------------------------

/// A cloneable byte sink shared between the terminal stack and the assertions.
#[derive(Clone, Default)]
pub(crate) struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    pub(crate) fn bytes(&self) -> Vec<u8> {
        lock(&self.0).clone()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        lock(&self.0).extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Scripted [`EventSource`] over a channel (the loop's poll deadline still elapses for real, so
/// the idle-wake timing in these tests is genuine).
pub(crate) struct ChannelEvents {
    rx: mpsc::Receiver<Event>,
    pending: Option<Event>,
}

impl ChannelEvents {
    pub(crate) fn new(rx: mpsc::Receiver<Event>) -> Self {
        Self { rx, pending: None }
    }
}

impl EventSource for ChannelEvents {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        if self.pending.is_some() {
            return Ok(true);
        }
        match self.rx.recv_timeout(timeout) {
            Ok(e) => {
                self.pending = Some(e);
                Ok(true)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                thread::sleep(timeout);
                Ok(false)
            }
        }
    }

    fn read(&mut self) -> io::Result<Event> {
        if let Some(e) = self.pending.take() {
            return Ok(e);
        }
        self.rx
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "no scripted event"))
    }
}

/// The key events crossterm 0.29 makes of the terminal's OSC 11 reply `ESC ] 1 1 ; <colour> <end>`
/// (`runtime::requery`): `Alt+]`, one character key a byte (an uppercase one with `Shift`), and for
/// the terminator `Ctrl+G` (`end` = BEL), `Alt+\` (`end` = ST) or nothing (`end` empty).
#[cfg(unix)]
pub(crate) fn reply_events(colour: &str, end: &str) -> Vec<Event> {
    let key = |c: char, m: KeyModifiers| Event::Key(KeyEvent::new(KeyCode::Char(c), m));
    let mut events = vec![key(']', KeyModifiers::ALT)];
    for c in "11;".chars().chain(colour.chars()) {
        let shift = if c.is_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        events.push(key(c, shift));
    }
    match end {
        "\x07" => events.push(key('g', KeyModifiers::CONTROL)),
        "\x1b\\" => events.push(key('\\', KeyModifiers::ALT)),
        "" => {}
        other => panic!("no such terminator: {other:?}"),
    }
    events
}

// --- the conversation's blocks ----------------------------------------------

/// `md` rendered (no color) into the blocks the terminal's markdown sink commits: text as rows,
/// each closed buffering block kept as its source (`markdown::Fragment`, laid out at every width).
pub(crate) fn markdown_blocks(md: &str) -> Vec<crate::ui::block::Block> {
    use crate::markdown::{CodeTheme, Fragment, RenderOptions, Sink, Writer};
    use crate::ui::block::Block;

    struct Keep(Arc<Mutex<Vec<Block>>>);
    impl Sink for Keep {
        fn write(&mut self, rendered: &str) {
            lock(&self.0).extend(Block::rows(rendered.lines().map(str::to_owned)));
        }
        fn write_block(&mut self, block: Fragment) {
            lock(&self.0).push(Block::laid(block));
        }
        fn width(&self) -> usize {
            80
        }
    }
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut w = Writer::new(
        Box::new(Keep(Arc::clone(&out))),
        RenderOptions {
            color: false,
            code_theme: CodeTheme::Monokai,
        },
    );
    w.write(md.as_bytes());
    w.flush();
    drop(w);
    std::mem::take(&mut *lock(&out))
}

/// A markdown reply streamed the way the terminal's markdown sink commits it (tui-plan S7b): text
/// as rows, an open buffering block as a live block committed again under one id each time it
/// grows, and once more when it closes. [`Streamed::feed`] writes source and hands back what was
/// committed meanwhile, for a test to push into a buffer step by step.
pub(crate) struct Streamed {
    w: crate::markdown::Writer,
    out: Arc<Mutex<Vec<crate::ui::block::Block>>>,
}

impl Streamed {
    /// A stream with no color (the assertions read the layout).
    pub(crate) fn new() -> Self {
        Self::with_color(false)
    }

    /// A stream rendering with `color` (on: code is highlighted, as a terminal shows it).
    pub(crate) fn with_color(color: bool) -> Self {
        use crate::markdown::{CodeTheme, Fragment, RenderOptions, Sink, Writer};
        use crate::ui::block::{Block, LiveId};

        struct Live {
            out: Arc<Mutex<Vec<Block>>>,
            id: Option<LiveId>,
        }
        impl Sink for Live {
            fn write(&mut self, rendered: &str) {
                lock(&self.out).extend(Block::rows(rendered.lines().map(str::to_owned)));
            }
            fn write_block(&mut self, block: Fragment) {
                let b = match self.id.take() {
                    Some(id) => Block::live(id, block, false),
                    None => Block::laid(block),
                };
                lock(&self.out).push(b);
            }
            fn width(&self) -> usize {
                0
            }
            fn shows_open_blocks(&self) -> bool {
                true
            }
            fn grow_block(&mut self, block: Fragment) {
                let id = *self.id.get_or_insert_with(LiveId::next);
                lock(&self.out).push(Block::live(id, block, true));
            }
        }
        let out = Arc::new(Mutex::new(Vec::new()));
        let w = Writer::new(
            Box::new(Live {
                out: Arc::clone(&out),
                id: None,
            }),
            RenderOptions {
                color,
                code_theme: CodeTheme::Monokai,
            },
        );
        Self { w, out }
    }

    /// Writes `md` and returns what it committed.
    pub(crate) fn feed(&mut self, md: &str) -> Vec<crate::ui::block::Block> {
        self.w.write(md.as_bytes());
        std::mem::take(&mut *lock(&self.out))
    }

    /// Ends the reply (the open block closes) and returns what that committed.
    pub(crate) fn end(&mut self) -> Vec<crate::ui::block::Block> {
        self.w.flush();
        std::mem::take(&mut *lock(&self.out))
    }
}

/// A test of this binary run again under `script(1)`, `--ignored` and alone: `script`'s pty is the
/// child's controlling terminal — a real `/dev/tty`, which a socket pair or a pty's own device
/// cannot stand in for (`ui::runtime::tty`) — and it answers nothing. `script`'s stdin is a pipe held
/// open and silent, so nothing is ever typed; killing `script` closes the pty's master end, which is
/// the terminal going away. `envs` tell the child where to [`report`]; a child run without them
/// must do nothing.
#[cfg(unix)]
pub(crate) fn under_script(
    child_test: &str,
    envs: &[(&str, &std::ffi::OsStr)],
) -> std::process::Child {
    under_script_to(child_test, envs, std::path::Path::new("/dev/null"))
}

/// [`under_script`], with `script`'s typescript — every byte the child wrote to its terminal — kept at
/// `typescript`.
#[cfg(unix)]
pub(crate) fn under_script_to(
    child_test: &str,
    envs: &[(&str, &std::ffi::OsStr)],
    typescript: &std::path::Path,
) -> std::process::Child {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().expect("the test binary");
    let child_args = ["--exact", child_test, "--ignored", "--test-threads=1"];
    let mut cmd = Command::new("script");
    if cfg!(target_os = "linux") {
        // util-linux: `script -qec <command> <file>`.
        let line = format!("'{}' {}", exe.display(), child_args.join(" "));
        cmd.args(["-qec", &line]).arg(typescript);
    } else {
        // BSD (macOS): `script -q <file> <command…>`.
        cmd.arg("-q").arg(typescript).arg(&exe).args(child_args);
    }
    cmd.envs(envs.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("script(1)")
}

/// The child of [`under_script`] says what happened: written beside `path` and renamed onto it, so
/// [`await_report`] finds the whole of it or nothing.
#[cfg(unix)]
pub(crate) fn report(path: &std::path::Path, what: &str) {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    std::fs::write(&part, what).expect("the report");
    std::fs::rename(&part, path).expect("the report, renamed into place");
}

/// What a child [`report`]ed at `path`, once it has; `None` when it has not within `within`.
#[cfg(unix)]
pub(crate) fn await_report(path: &std::path::Path, within: Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + within;
    loop {
        if let Ok(said) = std::fs::read_to_string(path) {
            return Some(said);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}
