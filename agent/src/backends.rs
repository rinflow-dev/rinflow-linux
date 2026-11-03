//! Keeps the input backends running for as long as the desktop allows:
//! starts them, reports their state to the window, and starts them again
//! after they are lost (compositor restarted, Steam Deck switched modes),
//! with a growing pause so a desktop without them is not hammered.

use rinflow_core::LcInputEvent;
use rinflow_shell::i18n::tr;
use rinflow_shell::state::{BackendState, BackendView, SharedState};
use rinflow_input::{
    CaptureCommand, CaptureEvent, ClipboardCommand, ClipboardEvent, ClipboardLink, EisSource,
    EmulationEvent,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

const FIRST_RETRY: Duration = Duration::from_secs(3);
const LONGEST_RETRY: Duration = Duration::from_secs(60);
/// How often to look for a compositor socket while nothing plays input.
const RESCAN: Duration = Duration::from_secs(5);

use crate::desktop::{self, Desktop};

/// The desktop now, from the agent's own environment and the systemd user
/// manager's (see `desktop.rs`).
async fn detect_desktop(bus: Option<&zbus::Connection>) -> Desktop {
    let manager = match bus {
        Some(bus) => desktop::manager_variables(bus).await,
        None => None,
    };
    desktop::decide(
        &desktop::own_variables(),
        manager.as_ref(),
        &desktop::runtime_directory(),
        desktop::is_socket,
        rinflow_input::ei_discover::find().is_some(),
    )
}

struct Capture {
    commands: mpsc::UnboundedSender<CaptureCommand>,
    task: JoinHandle<()>,
}

struct Emulation {
    input: mpsc::UnboundedSender<LcInputEvent>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    /// The session's clipboard (portal sessions only).
    clipboard: Option<mpsc::UnboundedSender<ClipboardCommand>>,
}

struct Retry {
    pause: Duration,
    due: Option<Instant>,
}

impl Retry {
    fn new() -> Self {
        Self { pause: FIRST_RETRY, due: Some(Instant::now()) }
    }

    fn later(&mut self) {
        self.due = Some(Instant::now() + self.pause);
        self.pause = (self.pause * 2).min(LONGEST_RETRY);
    }

    fn now(&mut self) {
        self.pause = FIRST_RETRY;
        self.due = Some(Instant::now());
    }

    fn succeeded(&mut self) {
        self.pause = FIRST_RETRY;
        self.due = None;
    }
}

pub struct Backends {
    state: SharedState,
    state_directory: PathBuf,
    desktop: Desktop,
    capture: Option<Capture>,
    capture_events: mpsc::UnboundedSender<CaptureEvent>,
    capture_retry: Retry,
    emulation: Option<Emulation>,
    emulation_events: mpsc::UnboundedSender<EmulationEvent>,
    emulation_retry: Retry,
    clipboard_events: mpsc::UnboundedSender<ClipboardEvent>,
    /// The user shares the clipboard: each new emulation session is told.
    clipboard_enabled: bool,
    /// The session bus, for the systemd manager's environment.
    bus: Option<zbus::Connection>,
    last_scan: Instant,
    /// When a peer's input was last played here (Unix milliseconds; 0
    /// never): an edge the pointer hits right after that is the peer's
    /// doing, not the user's.
    pub last_played: Arc<AtomicU64>,
    /// The last absolute move a peer sent (where its pointer entered), to
    /// play again if this device's own capture swallowed it.
    last_entry: Option<(Instant, LcInputEvent)>,
}

impl Backends {
    pub fn new(
        state: SharedState,
        state_directory: PathBuf,
        capture_events: mpsc::UnboundedSender<CaptureEvent>,
        emulation_events: mpsc::UnboundedSender<EmulationEvent>,
        clipboard_events: mpsc::UnboundedSender<ClipboardEvent>,
    ) -> Self {
        let desktop = desktop::decide(
            &desktop::own_variables(),
            None,
            &desktop::runtime_directory(),
            desktop::is_socket,
            rinflow_input::ei_discover::find().is_some(),
        );
        log::info!("desktop: {desktop:?}");
        let backends = Self {
            state,
            state_directory,
            desktop,
            capture: None,
            capture_events,
            capture_retry: Retry::new(),
            emulation: None,
            emulation_events,
            emulation_retry: Retry::new(),
            clipboard_events,
            clipboard_enabled: false,
            bus: None,
            // The first poll looks at the systemd manager's environment too.
            last_scan: Instant::now() - RESCAN,
            last_played: Arc::new(AtomicU64::new(0)),
            last_entry: None,
        };
        backends.describe_desktop();
        backends
    }

    /// The capture backend's command channel while one runs.
    pub fn capture_commands(&self) -> Option<mpsc::UnboundedSender<CaptureCommand>> {
        self.capture.as_ref().map(|capture| capture.commands.clone())
    }

    /// Plays a peer's input here, if anything can.
    pub fn play(&mut self, event: LcInputEvent) {
        if event.kind == rinflow_core::LC_INPUT_MOUSE_MOVE
            && event.flags & rinflow_core::LC_INPUT_FLAG_ABSOLUTE_POSITION != 0
        {
            self.last_entry = Some((Instant::now(), event));
        }
        if let Some(emulation) = &self.emulation {
            let _ = emulation.input.send(event);
        }
    }

    /// Offers a peer's clipboard to this desktop's applications.
    pub fn offer_clipboard(&self, clipboard: rinflow_core::Clipboard) {
        if let Some(commands) = self.emulation.as_ref().and_then(|e| e.clipboard.as_ref()) {
            let _ = commands.send(ClipboardCommand::Offer(clipboard));
        }
    }

    /// Shares this desktop's clipboard, or stops reading it at all.
    pub fn set_clipboard_enabled(&mut self, enabled: bool) {
        self.clipboard_enabled = enabled;
        if let Some(commands) = self.emulation.as_ref().and_then(|e| e.clipboard.as_ref()) {
            let _ = commands.send(ClipboardCommand::Enable(enabled));
        }
    }

    /// Plays the peer's entry move again. When the pointer sat on one of
    /// this device's own edges (Mutter activates a barrier even for a warp
    /// off it), the compositor handed the entry to this device's capture
    /// instead of moving the pointer; after the release it goes again.
    pub fn replay_entry(&mut self) {
        if let Some((when, event)) = self.last_entry
            && when.elapsed() < Duration::from_secs(2)
        {
            log::debug!("replaying the peer's entry move after an edge hit of its own");
            self.play(event);
        }
    }

    /// Starts whatever is due. Call about once a second; returns true when
    /// a capture backend was just started (its command channel is new).
    pub async fn poll(&mut self) -> bool {
        let now = Instant::now();
        let mut capture_started = false;
        if self.desktop == Desktop::None || now.duration_since(self.last_scan) >= RESCAN {
            self.last_scan = now;
            if self.bus.is_none() {
                self.bus = zbus::Connection::session().await.ok();
            }
            let desktop = detect_desktop(self.bus.as_ref()).await;
            if desktop != self.desktop {
                log::info!("desktop changed: {:?} → {desktop:?}", self.desktop);
                self.desktop = desktop;
                self.stop_capture();
                self.stop_emulation();
                self.capture_retry.now();
                self.emulation_retry.now();
                self.describe_desktop();
            }
        }
        if self.capture.is_none()
            && self.capture_retry.due.is_some_and(|due| now >= due)
            && self.desktop == Desktop::Wayland
        {
            self.start_capture();
            capture_started = true;
        }
        if self.emulation.is_none()
            && self.emulation_retry.due.is_some_and(|due| now >= due)
            && let Some(source) = self.emulation_source()
        {
            self.start_emulation(source);
        }
        capture_started
    }

    /// The user asks again (after declining, or when nothing came up).
    pub fn retry(&mut self) {
        self.stop_capture();
        self.stop_emulation();
        self.capture_retry.now();
        self.emulation_retry.now();
        self.state.set_capture(waiting());
        self.state.set_emulation(waiting());
    }

    pub fn on_capture_event(&mut self, event: &CaptureEvent) {
        match event {
            CaptureEvent::Ready => {
                self.capture_retry.succeeded();
                self.state.set_capture(BackendView {
                    state: BackendState::Ready,
                    backend: "portal".into(),
                    detail: String::new(),
                });
            }
            CaptureEvent::Prompting => self.state.set_capture(BackendView {
                state: BackendState::Prompting,
                backend: "portal".into(),
                detail: tr(
                    "桌面正在询问是否允许 Rinflow 捕获输入",
                    "The desktop is asking whether Rinflow may capture input",
                )
                .into(),
            }),
            CaptureEvent::Denied(detail) => {
                log::warn!("capture denied: {detail}");
                self.stop_capture();
                self.capture_retry.due = None;
                self.state.set_capture(BackendView {
                    state: BackendState::Denied,
                    backend: "portal".into(),
                    detail: tr(
                        "没有获得捕获输入的许可。本机不能控制其他设备，直到重新请求并允许。",
                        "Permission to capture input was not given. This device can't control others until it is requested again and allowed.",
                    )
                    .into(),
                });
            }
            CaptureEvent::Lost(detail) => {
                log::warn!("capture lost: {detail}");
                self.stop_capture();
                self.capture_retry.later();
                self.state.set_capture(BackendView {
                    state: BackendState::Waiting,
                    backend: "portal".into(),
                    detail: detail.clone(),
                });
            }
            _ => {}
        }
    }

    pub fn on_emulation_event(&mut self, event: &EmulationEvent) {
        let backend = self.emulation_backend_name();
        match event {
            EmulationEvent::Ready => {
                self.emulation_retry.succeeded();
                self.state.set_emulation(BackendView {
                    state: BackendState::Ready,
                    backend,
                    detail: String::new(),
                });
            }
            EmulationEvent::Prompting => self.state.set_emulation(BackendView {
                state: BackendState::Prompting,
                backend,
                detail: tr(
                    "桌面正在询问是否允许 Rinflow 控制本机",
                    "The desktop is asking whether Rinflow may control this device",
                )
                .into(),
            }),
            EmulationEvent::Denied(detail) => {
                log::warn!("emulation denied: {detail}");
                self.stop_emulation();
                self.emulation_retry.due = None;
                self.state.set_emulation(BackendView {
                    state: BackendState::Denied,
                    backend,
                    detail: tr(
                        "没有获得控制本机的许可。其他设备不能控制本机，直到重新请求并允许。",
                        "Permission to control this device was not given. Other devices can't control it until it is requested again and allowed.",
                    )
                    .into(),
                });
            }
            EmulationEvent::Lost(detail) => {
                log::warn!("emulation lost: {detail}");
                self.stop_emulation();
                self.emulation_retry.later();
                self.state.set_emulation(BackendView {
                    state: BackendState::Waiting,
                    backend,
                    detail: detail.clone(),
                });
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.stop_capture();
        self.stop_emulation();
    }

    fn emulation_source(&self) -> Option<EisSource> {
        match self.desktop {
            Desktop::Gamescope => rinflow_input::ei_discover::find().map(EisSource::Socket),
            Desktop::Wayland => Some(EisSource::Portal),
            Desktop::X11 | Desktop::None => None,
        }
    }

    fn emulation_backend_name(&self) -> String {
        match self.desktop {
            Desktop::Gamescope => "gamescope",
            _ => "portal",
        }
        .into()
    }

    fn start_capture(&mut self) {
        let (commands, receiver) = mpsc::unbounded_channel();
        let events = self.capture_events.clone();
        let directory = self.state_directory.clone();
        let task = tokio::task::spawn_local(async move {
            rinflow_input::portal_capture::run(&directory, receiver, events).await;
        });
        self.capture = Some(Capture { commands, task });
        self.state.set_capture(BackendView {
            state: BackendState::Waiting,
            backend: "portal".into(),
            detail: tr("正在连接桌面的输入捕获门户…", "Connecting to the desktop's input capture portal…").into(),
        });
    }

    fn stop_capture(&mut self) {
        if let Some(capture) = self.capture.take() {
            let _ = capture.commands.send(CaptureCommand::Shutdown);
            capture.task.abort();
        }
    }

    fn start_emulation(&mut self, source: EisSource) {
        let (input, receiver) = mpsc::unbounded_channel();
        let (shutdown, shutdown_rx) = oneshot::channel();
        let events = self.emulation_events.clone();
        let directory = self.state_directory.clone();
        let last_played = self.last_played.clone();
        // The clipboard comes with the RemoteDesktop portal's session;
        // gamescope has none to give.
        let (clipboard, link) = if source == EisSource::Portal {
            let (commands, commands_rx) = mpsc::unbounded_channel();
            let _ = commands.send(ClipboardCommand::Enable(self.clipboard_enabled));
            let link = ClipboardLink { commands: commands_rx, events: self.clipboard_events.clone() };
            (Some(commands), Some(link))
        } else {
            (None, None)
        };
        log::info!("starting emulation from {source:?}");
        let task = tokio::task::spawn_local(async move {
            rinflow_input::ei_emulation::run(source, &directory, receiver, events, shutdown_rx, last_played, link).await;
        });
        self.emulation = Some(Emulation { input, shutdown: Some(shutdown), task, clipboard });
        self.state.set_emulation(BackendView {
            state: BackendState::Waiting,
            backend: self.emulation_backend_name(),
            detail: tr("正在连接输入注入…", "Connecting input playback…").into(),
        });
    }

    fn stop_emulation(&mut self) {
        self.state.set_clipboard_available(false);
        if let Some(mut emulation) = self.emulation.take() {
            if let Some(shutdown) = emulation.shutdown.take() {
                let _ = shutdown.send(());
            }
            // Give it a moment to release held keys; then it is gone.
            let task = emulation.task;
            tokio::task::spawn_local(async move {
                let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
            });
        }
    }

    /// What this desktop can and cannot do, before anything is tried.
    fn describe_desktop(&self) {
        match self.desktop {
            Desktop::Wayland => {
                self.state.set_capture(waiting());
                self.state.set_emulation(waiting());
            }
            Desktop::Gamescope => {
                self.state.set_capture(BackendView {
                    state: BackendState::Unavailable,
                    backend: "gamescope".into(),
                    detail: tr(
                        "游戏模式下本机只能被其他设备控制，不能控制别的设备。",
                        "In Gaming Mode this device can be controlled by others, but can't control them.",
                    )
                    .into(),
                });
                self.state.set_emulation(waiting());
            }
            Desktop::X11 => {
                let detail = tr(
                    "X11 会话暂不支持；请使用 Wayland 会话。",
                    "X11 sessions aren't supported yet; use a Wayland session.",
                );
                self.state.set_capture(unavailable("x11", detail));
                self.state.set_emulation(unavailable("x11", detail));
            }
            Desktop::None => {
                let detail = tr("没有找到图形会话。", "No graphical session was found.");
                self.state.set_capture(unavailable("", detail));
                self.state.set_emulation(unavailable("", detail));
            }
        }
    }
}

fn waiting() -> BackendView {
    BackendView {
        state: BackendState::Waiting,
        backend: String::new(),
        detail: String::new(),
    }
}

fn unavailable(backend: &str, detail: &str) -> BackendView {
    BackendView {
        state: BackendState::Unavailable,
        backend: backend.into(),
        detail: detail.into(),
    }
}
