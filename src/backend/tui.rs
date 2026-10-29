use std::error::Error;
use std::io::stdout;
use std::time::Duration;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture, KeyEvent, MouseEvent};
use crossterm::execute;
use futures::{FutureExt, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui_image::picker::{Picker, ProtocolType};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use super::manga_provider::{ChapterToRead, Manga, MangaProvider};
use super::tracker::MangaTracker;
use crate::view::app::{App, AppState, MangaToRead};
use crate::view::widgets::Component;

pub enum Action {
    Quit,
}

/// These are the events this app will listen to
#[derive(Clone, Debug, PartialEq)]
pub enum Events {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    GoToMangaPage(Manga),
    GoBackMangaPage,
    GoToHome,
    GoSearchPage,
    GoStatsPage,
    LocalLibraryReloaded,
    Error(String),
    ReadChapter(ChapterToRead, MangaToRead),
}

/// Puts the terminal back into the state it had before the app started.
///
/// Mouse capture is disabled first and foremost: `ratatui::restore` only disables raw mode and
/// leaves the alternate screen, it knows nothing about the mouse reporting that crossterm enabled.
/// If mouse capture is left on, every mouse movement after the app exits is reported as an escape
/// sequence (for example `\x1b[<35;10;5M`) and that is printed by the shell as control characters.
pub fn restore_terminal() {
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
}

/// Owns the terminal for as long as the app is running and restores it on `Drop`.
///
/// Restoring on `Drop` means the terminal is cleaned up on every exit path: when `main` returns
/// early because of a `?` error, when a panic unwinds the stack, and on a normal quit.
pub struct TerminalSession {
    restored: bool,
}

impl TerminalSession {
    /// Enables mouse capture and installs a panic hook that restores the terminal.
    ///
    /// Call this before `ratatui::init` so that a failure while initializing the terminal is also
    /// cleaned up by this guard.
    pub fn start() -> std::io::Result<(Self, ratatui::DefaultTerminal)> {
        execute!(stdout(), EnableMouseCapture)?;

        // `ratatui::init` installs its own panic hook that disables raw mode and leaves the
        // alternate screen, but it does not disable mouse capture, so on its own it leaves the
        // terminal reporting mouse events. Wrap whatever hook is installed so that mouse capture
        // (and the rest of the terminal state) is always restored before the panic is reported.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            restore_terminal();
            previous_hook(panic_info);
        }));

        let terminal = ratatui::init();

        Ok((Self { restored: false }, terminal))
    }

    pub fn restore(&mut self) {
        if self.restored {
            return;
        }

        self.restored = true;
        restore_terminal();
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
fn get_picker() -> Option<Picker> {
    Picker::from_query_stdio()
        .ok()
        .filter(|picker| picker.protocol_type() != ProtocolType::Halfblocks)
}

#[cfg(target_os = "windows")]
fn get_picker() -> Option<Picker> {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;

    struct FontSize {
        pub width: u16,
        pub height: u16,
    }
    impl Default for FontSize {
        fn default() -> Self {
            FontSize {
                width: 17,
                height: 38,
            }
        }
    }

    let size: FontSize = match unsafe { GetDpiForWindow(GetConsoleWindow()) } {
        96 => FontSize {
            width: 9,
            height: 20,
        },
        120 => FontSize {
            width: 12,
            height: 25,
        },
        144 => FontSize {
            width: 14,
            height: 32,
        },
        _ => FontSize::default(),
    };

    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

    if picker.protocol_type() == ProtocolType::Halfblocks {
        return None;
    }
    Some(picker)
}

///Start app's main loop
pub async fn run_app<T, B, S>(
    mut terminal: Terminal<B>,
    api_client: T,
    manga_tracker: Option<S>,
    filter_state: T::FiltersHandler,
    filter_widget: T::Widget,
) -> Result<(), Box<dyn Error>>
where
    T: MangaProvider,
    B: Backend,
    B::Error: 'static,
    S: MangaTracker,
{
    let mut app = App::new(api_client, manga_tracker, get_picker(), filter_state, filter_widget);

    let tick_rate = std::time::Duration::from_millis(250);

    let main_event_handle = handle_events(tick_rate, app.global_event_tx.clone());

    while app.state == AppState::Runnning {
        terminal.draw(|f| {
            app.render(f.area(), f);
        })?;

        app.listen_to_event().await;

        app.update_based_on_action();
    }

    main_event_handle.abort();

    Ok(())
}

pub fn handle_events(tick_rate: Duration, event_tx: UnboundedSender<Events>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = crossterm::event::EventStream::new();
        let mut tick_interval = tokio::time::interval(tick_rate);

        loop {
            let delay = tick_interval.tick();
            let event = reader.next().fuse();
            tokio::select! {
                maybe_event = event => {
                    match maybe_event  {
                        Some(Ok(evt)) => {
                            match evt {
                                crossterm::event::Event::Key(key) => {
                                    if key.kind == crossterm::event::KeyEventKind::Press {
                                        event_tx.send(Events::Key(key)).ok();
                                    }
                                },
                                crossterm::event::Event::Mouse(mouse_event) => {
                                    event_tx.send(Events::Mouse(mouse_event)).ok();
                                }
                                _ => {}
                            }
                        }
                        Some(Err(_)) => {

                        }
                        None => {}

                    }

                }
                    _ = delay => {
                        event_tx.send(Events::Tick).ok();
                    }
            }
        }
    })
}
