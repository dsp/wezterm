use crate::mux::domain::DomainId;
use crate::mux::renderable::Renderable;
use crate::mux::tab::{alloc_tab_id, Tab, TabId};
use crate::server::codec::*;
use crate::server::domain::ClientInner;
use failure::Fallible;
use filedescriptor::Pipe;
use log::error;
use portable_pty::PtySize;
use promise::Future;
use std::cell::RefCell;
use std::cell::RefMut;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};
use term::color::ColorPalette;
use term::{CursorPosition, Line};
use term::{KeyCode, KeyModifiers, MouseEvent, TerminalHost};
use termwiz::hyperlink::Hyperlink;
use termwiz::input::KeyEvent;

pub struct ClientTab {
    client: Arc<ClientInner>,
    local_tab_id: TabId,
    remote_tab_id: TabId,
    renderable: RefCell<RenderableState>,
    writer: RefCell<TabWriter>,
    reader: Pipe,
}

impl ClientTab {
    pub fn new(client: &Arc<ClientInner>, remote_tab_id: TabId) -> Self {
        let local_tab_id = alloc_tab_id();
        let writer = TabWriter {
            client: Arc::clone(client),
            remote_tab_id,
        };
        let render = RenderableState {
            client: Arc::clone(client),
            remote_tab_id,
            coarse: RefCell::new(None),
            last_poll: RefCell::new(Instant::now()),
            dirty_all: RefCell::new(true),
            dead: RefCell::new(false),
            poll_future: RefCell::new(None),
        };

        let reader = Pipe::new().expect("Pipe::new failed");

        Self {
            client: Arc::clone(client),
            remote_tab_id,
            local_tab_id,
            renderable: RefCell::new(render),
            writer: RefCell::new(writer),
            reader,
        }
    }
}

impl Tab for ClientTab {
    fn tab_id(&self) -> TabId {
        self.local_tab_id
    }
    fn renderer(&self) -> RefMut<dyn Renderable> {
        self.renderable.borrow_mut()
    }

    fn get_title(&self) -> String {
        let renderable = self.renderable.borrow();
        let coarse = renderable.coarse.borrow();
        format!(
            "[muxed] {}",
            coarse
                .as_ref()
                .map(|coarse| coarse.title.as_str())
                .unwrap_or("")
        )
    }

    fn send_paste(&self, text: &str) -> Fallible<()> {
        let mut client = self.client.client.lock().unwrap();
        client.send_paste(SendPaste {
            tab_id: self.remote_tab_id,
            data: text.to_owned(),
        });
        Ok(())
    }

    fn reader(&self) -> Fallible<Box<dyn std::io::Read + Send>> {
        error!("made reader for ClientTab");
        Ok(Box::new(self.reader.read.try_clone()?))
    }

    fn writer(&self) -> RefMut<dyn std::io::Write> {
        self.writer.borrow_mut()
    }

    fn resize(&self, size: PtySize) -> Fallible<()> {
        let mut client = self.client.client.lock().unwrap();
        client.resize(Resize {
            tab_id: self.remote_tab_id,
            size,
        });
        Ok(())
    }

    fn key_down(&self, key: KeyCode, mods: KeyModifiers) -> Fallible<()> {
        let mut client = self.client.client.lock().unwrap();
        client.key_down(SendKeyDown {
            tab_id: self.remote_tab_id,
            event: KeyEvent {
                key,
                modifiers: mods,
            },
        });
        Ok(())
    }

    fn mouse_event(&self, event: MouseEvent, host: &mut dyn TerminalHost) -> Fallible<()> {
        let mut client = self.client.client.lock().unwrap();
        let resp = client
            .mouse_event(SendMouseEvent {
                tab_id: self.remote_tab_id,
                event,
            })
            .wait()?;

        if resp.clipboard.is_some() {
            host.set_clipboard(resp.clipboard)?;
        }

        Ok(())
    }

    fn advance_bytes(&self, _buf: &[u8], _host: &mut dyn TerminalHost) {
        panic!("ClientTab::advance_bytes not impl");
    }

    // clippy is wrong: the borrow checker hates returning the value directly
    #[allow(clippy::let_and_return)]
    fn is_dead(&self) -> bool {
        let renderable = self.renderable.borrow();
        let dead = *renderable.dead.borrow();
        dead
    }

    fn palette(&self) -> ColorPalette {
        Default::default()
    }

    fn domain_id(&self) -> DomainId {
        self.client.local_domain_id
    }
}

struct RenderableState {
    client: Arc<ClientInner>,
    remote_tab_id: TabId,
    coarse: RefCell<Option<GetCoarseTabRenderableDataResponse>>,
    last_poll: RefCell<Instant>,
    dirty_all: RefCell<bool>,
    dead: RefCell<bool>,
    poll_future: RefCell<Option<Future<GetCoarseTabRenderableDataResponse>>>,
}

const POLL_INTERVAL: Duration = Duration::from_millis(50);

impl RenderableState {
    fn poll(&self) -> Fallible<()> {
        let ready = self
            .poll_future
            .borrow()
            .as_ref()
            .map(Future::is_ready)
            .unwrap_or(false);
        if ready {
            let coarse = self.poll_future.borrow_mut().take().unwrap().wait()?;
            self.coarse.borrow_mut().replace(coarse);
            log::trace!(
                "poll: got coarse data in {:?}",
                self.last_poll.borrow().elapsed()
            );
            *self.last_poll.borrow_mut() = Instant::now();
        } else if self.poll_future.borrow().is_some() {
            // We have a poll in progress
            return Ok(());
        }

        let dirty_all = *self.dirty_all.borrow();

        if !dirty_all {
            let last = *self.last_poll.borrow();
            if last.elapsed() < POLL_INTERVAL {
                return Ok(());
            }
        }

        {
            let mut client = self.client.client.lock().unwrap();
            *self.poll_future.borrow_mut() = Some(client.get_coarse_tab_renderable_data(
                GetCoarseTabRenderableData {
                    tab_id: self.remote_tab_id,
                    dirty_all,
                },
            ));
        }
        *self.dirty_all.borrow_mut() = false;
        Ok(())
    }
}

impl Renderable for RenderableState {
    fn get_cursor_position(&self) -> CursorPosition {
        let coarse = self.coarse.borrow();
        if let Some(coarse) = coarse.as_ref() {
            coarse.cursor_position
        } else {
            CursorPosition::default()
        }
    }

    fn get_dirty_lines(&self) -> Vec<(usize, Line, Range<usize>)> {
        let coarse = self.coarse.borrow();
        if let Some(coarse) = coarse.as_ref() {
            coarse
                .dirty_lines
                .iter()
                .map(|dl| {
                    (
                        dl.line_idx,
                        dl.line.clone(),
                        dl.selection_col_from..dl.selection_col_to,
                    )
                })
                .collect()
        } else {
            vec![]
        }
    }

    fn has_dirty_lines(&self) -> bool {
        if self.poll().is_err() {
            *self.dead.borrow_mut() = true;
        }

        let coarse = self.coarse.borrow();
        if let Some(coarse) = coarse.as_ref() {
            !coarse.dirty_lines.is_empty()
        } else {
            false
        }
    }

    fn make_all_lines_dirty(&mut self) {
        *self.dirty_all.borrow_mut() = true;
    }

    fn clean_dirty_lines(&mut self) {
        if let Some(c) = self.coarse.borrow_mut().as_mut() {
            c.dirty_lines.clear()
        }
    }

    fn current_highlight(&self) -> Option<Arc<Hyperlink>> {
        let coarse = self.coarse.borrow();
        coarse
            .as_ref()
            .and_then(|coarse| coarse.current_highlight.clone())
    }

    fn physical_dimensions(&self) -> (usize, usize) {
        let coarse = self.coarse.borrow();
        if let Some(coarse) = coarse.as_ref() {
            (coarse.physical_rows, coarse.physical_cols)
        } else {
            (24, 80)
        }
    }
}

struct TabWriter {
    client: Arc<ClientInner>,
    remote_tab_id: TabId,
}

impl std::io::Write for TabWriter {
    fn write(&mut self, data: &[u8]) -> Result<usize, std::io::Error> {
        let mut client = self.client.client.lock().unwrap();
        client
            .write_to_tab(WriteToTab {
                tab_id: self.remote_tab_id,
                data: data.to_vec(),
            })
            .wait()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e)))?;
        Ok(data.len())
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        Ok(())
    }
}
