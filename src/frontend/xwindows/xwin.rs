use super::xkeysyms;
use super::{Connection, Window};
use crate::config::Config;
use crate::font::FontConfiguration;
use crate::frontend::guicommon::host::{HostHelper, HostImpl, TabHost};
use crate::frontend::guicommon::window::{Dimensions, TerminalWindow};
use crate::frontend::xwindows::x11loop::{GuiEventLoop, WindowId as X11WindowId};
use crate::mux::tab::{Tab, TabId};
use crate::mux::window::WindowId;
use crate::mux::{Mux, SessionTerminated};
use crate::opengl::render::Renderer;
use failure::Error;
use std::rc::Rc;
use std::sync::Arc;
use term::{self, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use xcb;

/// Holds most of the information we need to implement `TerminalHost`
struct Host {
    window: Window,
    event_loop: Rc<GuiEventLoop>,
    fonts: Rc<FontConfiguration>,
    config: Arc<Config>,
}

impl HostHelper for Host {
    fn with_window<F: Send + 'static + Fn(&mut TerminalWindow) -> Result<(), Error>>(
        &self,
        func: F,
    ) {
        let window_id = self.window.window.window_id;
        self.event_loop.with_window(window_id, func).ok();
    }

    fn toggle_full_screen(&mut self) {}
}

pub struct X11TerminalWindow {
    host: HostImpl<Host>,
    conn: Rc<Connection>,
    renderer: Renderer,
    width: u16,
    height: u16,
    cell_height: usize,
    cell_width: usize,
    have_pending_resize: Option<(u16, u16)>,
    mux_window_id: WindowId,
}

impl TerminalWindow for X11TerminalWindow {
    fn get_mux_window_id(&self) -> WindowId {
        self.mux_window_id
    }
    fn config(&self) -> &Arc<Config> {
        &self.host.config
    }
    fn fonts(&self) -> &Rc<FontConfiguration> {
        &self.host.fonts
    }

    fn set_window_title(&mut self, title: &str) -> Result<(), Error> {
        self.host.window.set_title(title);
        Ok(())
    }
    fn frame(&self) -> glium::Frame {
        self.host.window.draw()
    }

    fn renderer(&mut self) -> &mut Renderer {
        &mut self.renderer
    }
    fn recreate_texture_atlas(&mut self, size: u32) -> Result<(), Error> {
        self.renderer.recreate_atlas(&self.host.window, size)
    }
    fn get_dimensions(&self) -> Dimensions {
        Dimensions {
            width: self.width,
            height: self.height,
            cell_height: self.cell_height,
            cell_width: self.cell_width,
        }
    }
    fn advise_renderer_that_scaling_has_changed(
        &mut self,
        cell_width: usize,
        cell_height: usize,
    ) -> Result<(), Error> {
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        self.renderer.scaling_changed(&self.host.window)
    }
    fn advise_renderer_of_resize(&mut self, width: u16, height: u16) -> Result<(), Error> {
        self.width = width;
        self.height = height;
        self.renderer.resize(&self.host.window, width, height)
    }
    fn resize_if_not_full_screen(&mut self, _width: u16, _height: u16) -> Result<bool, Error> {
        // FIXME: it would be nice to implement this!
        // It requires some plumbing to allow sending xcb_configure_window with
        // XCB_CONFIG_WINDOW_WIDTH and XCB_CONFIG_WINDOW_HEIGHT set.
        Ok(false)
    }

    fn check_for_resize(&mut self) -> Result<(), Error> {
        if let Some((width, height)) = self.have_pending_resize.take() {
            self.resize_surfaces(width, height, false)?;
        }
        Ok(())
    }
}

impl X11TerminalWindow {
    pub fn new(
        event_loop: &Rc<GuiEventLoop>,
        fonts: &Rc<FontConfiguration>,
        config: &Arc<Config>,
        tab: &Rc<Tab>,
    ) -> Result<X11TerminalWindow, Error> {
        let palette = config
            .colors
            .as_ref()
            .map(|p| p.clone().into())
            .unwrap_or_else(term::color::ColorPalette::default);

        let (physical_rows, physical_cols) = tab.renderer().physical_dimensions();

        let metrics = fonts.default_font_metrics()?;
        let (cell_height, cell_width) = (
            metrics.cell_height.ceil() as usize,
            metrics.cell_width.ceil() as usize,
        );

        let width = cell_width * physical_cols;
        let height = cell_height * physical_rows;

        let width = width as u16;
        let height = height as u16;
        let window = Window::new(&event_loop.conn, width, height)?;
        window.set_title("wezterm");

        let host = HostImpl::new(Host {
            window,
            event_loop: Rc::clone(event_loop),
            config: Arc::clone(config),
            fonts: Rc::clone(fonts),
        });

        let renderer = Renderer::new(&host.window, width, height, fonts, palette)?;
        let mux = Mux::get().unwrap();
        let mux_window_id = mux.add_new_window_with_tab(tab)?;
        host.window.show();

        Ok(X11TerminalWindow {
            host,
            renderer,
            conn: Rc::clone(&event_loop.conn),
            width,
            height,
            cell_height,
            cell_width,
            have_pending_resize: None,
            mux_window_id,
        })
    }

    pub fn window_id(&self) -> X11WindowId {
        self.host.window.window.window_id
    }

    pub fn expose(&mut self, _x: u16, _y: u16, _width: u16, _height: u16) -> Result<(), Error> {
        self.paint()
    }

    fn decode_key(&self, event: &xcb::KeyPressEvent) -> Option<(KeyCode, KeyModifiers)> {
        self.conn.xkb_lookup_keysym(event)
    }

    fn mouse_event(&mut self, event: MouseEvent) -> Result<(), Error> {
        let mux = Mux::get().unwrap();
        let tab = match mux.get_active_tab_for_window(self.get_mux_window_id()) {
            Some(tab) => tab,
            None => return Ok(()),
        };
        tab.mouse_event(event, &mut TabHost::new(&mut *tab.writer(), &mut self.host))?;
        Ok(())
    }

    pub fn dispatch_event(&mut self, event: &xcb::GenericEvent) -> Result<(), Error> {
        let r = event.response_type() & 0x7f;
        match r {
            xcb::EXPOSE => {
                let expose: &xcb::ExposeEvent = unsafe { xcb::cast_event(event) };
                self.expose(expose.x(), expose.y(), expose.width(), expose.height())?;
            }
            xcb::CONFIGURE_NOTIFY => {
                let cfg: &xcb::ConfigureNotifyEvent = unsafe { xcb::cast_event(event) };
                let schedule = self.have_pending_resize.is_none();
                self.have_pending_resize = Some((cfg.width(), cfg.height()));
                if schedule {
                    self.host.with_window(|win| win.check_for_resize());
                }
            }
            xcb::KEY_PRESS => {
                let key_press: &xcb::KeyPressEvent = unsafe { xcb::cast_event(event) };
                let mux = Mux::get().unwrap();
                let tab = match mux.get_active_tab_for_window(self.get_mux_window_id()) {
                    Some(tab) => tab,
                    None => return Ok(()),
                };
                if let Some((code, mods)) = self.decode_key(key_press) {
                    if mods == KeyModifiers::SUPER && code == KeyCode::Char('n') {
                        self.host
                            .event_loop
                            .schedule_spawn_new_window(&self.host.config);
                        return Ok(());
                    }

                    if self.host.process_gui_shortcuts(&*tab, mods, code)? {
                        return Ok(());
                    }

                    tab.key_down(code, mods)?;
                }
            }
            xcb::MOTION_NOTIFY => {
                let motion: &xcb::MotionNotifyEvent = unsafe { xcb::cast_event(event) };

                let event = MouseEvent {
                    kind: MouseEventKind::Move,
                    button: MouseButton::None,
                    x: (motion.event_x() as usize / self.cell_width) as usize,
                    y: (motion.event_y() as usize / self.cell_height) as i64,
                    modifiers: xkeysyms::modifiers_from_state(motion.state()),
                };
                self.mouse_event(event)?;
            }
            xcb::BUTTON_PRESS | xcb::BUTTON_RELEASE => {
                let button_press: &xcb::ButtonPressEvent = unsafe { xcb::cast_event(event) };

                let event = MouseEvent {
                    kind: match r {
                        xcb::BUTTON_PRESS => MouseEventKind::Press,
                        xcb::BUTTON_RELEASE => MouseEventKind::Release,
                        _ => unreachable!("button event mismatch"),
                    },
                    x: (button_press.event_x() as usize / self.cell_width) as usize,
                    y: (button_press.event_y() as usize / self.cell_height) as i64,
                    button: match button_press.detail() {
                        1 => MouseButton::Left,
                        2 => MouseButton::Middle,
                        3 => MouseButton::Right,
                        4 => MouseButton::WheelUp,
                        5 => MouseButton::WheelDown,
                        _ => {
                            eprintln!("button {} is not implemented", button_press.detail());
                            return Ok(());
                        }
                    },
                    modifiers: xkeysyms::modifiers_from_state(button_press.state()),
                };

                self.mouse_event(event)?;
            }
            xcb::CLIENT_MESSAGE => {
                let msg: &xcb::ClientMessageEvent = unsafe { xcb::cast_event(event) };
                println!("CLIENT_MESSAGE {:?}", msg.data().data32());
                if msg.data().data32()[0] == self.conn.atom_delete() {
                    return Err(SessionTerminated::WindowClosed.into());
                }
            }
            _ => {}
        }
        Ok(())
    }
}
