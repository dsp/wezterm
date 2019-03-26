use crate::config::Config;
use crate::font::FontConfiguration;
use crate::mux::tab::Tab;
use crate::mux::Mux;
use failure::Error;
use lazy_static::lazy_static;
use promise::Executor;
use serde_derive::*;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub mod glium;
pub mod guicommon;
pub mod muxserver;
#[cfg(all(unix, not(feature = "force-glutin"), not(target_os = "macos")))]
pub mod xwindows;

#[derive(Debug, Deserialize, Clone, Copy)]
pub enum FrontEndSelection {
    Glutin,
    X11,
    MuxServer,
    Null,
}

impl Default for FrontEndSelection {
    fn default() -> Self {
        if cfg!(feature = "force-glutin") {
            FrontEndSelection::Glutin
        } else if cfg!(all(unix, not(target_os = "macos"))) {
            FrontEndSelection::X11
        } else {
            FrontEndSelection::Glutin
        }
    }
}

lazy_static! {
    static ref EXECUTOR: Mutex<Option<Box<Executor>>> = Mutex::new(None);
}

pub fn gui_executor() -> Option<Box<Executor>> {
    let locked = EXECUTOR.lock().unwrap();
    match locked.as_ref() {
        Some(exec) => Some(exec.clone_executor()),
        None => None,
    }
}

impl FrontEndSelection {
    pub fn try_new(self, mux: &Rc<Mux>) -> Result<Rc<FrontEnd>, Error> {
        let front_end = match self {
            FrontEndSelection::Glutin => glium::glutinloop::GlutinFrontEnd::try_new(mux),
            #[cfg(all(unix, not(target_os = "macos")))]
            FrontEndSelection::X11 => xwindows::x11loop::X11FrontEnd::try_new(mux),
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            FrontEndSelection::X11 => bail!("X11 not compiled in"),
            FrontEndSelection::MuxServer => muxserver::MuxServerFrontEnd::try_new(mux),
            FrontEndSelection::Null => muxserver::MuxServerFrontEnd::new_null(mux),
        }?;

        EXECUTOR.lock().unwrap().replace(front_end.gui_executor());

        Ok(front_end)
    }

    // TODO: find or build a proc macro for this
    pub fn variants() -> Vec<&'static str> {
        vec!["Glutin", "X11", "MuxServer", "Null"]
    }
}

impl std::str::FromStr for FrontEndSelection {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "glutin" => Ok(FrontEndSelection::Glutin),
            "x11" => Ok(FrontEndSelection::X11),
            "muxserver" => Ok(FrontEndSelection::MuxServer),
            "null" => Ok(FrontEndSelection::Null),
            _ => Err(format_err!(
                "{} is not a valid FrontEndSelection variant, possible values are {:?}",
                s,
                FrontEndSelection::variants()
            )),
        }
    }
}

pub trait FrontEnd {
    /// Run the event loop.  Does not return until there is either a fatal
    /// error, or until there are no more windows left to manage.
    fn run_forever(&self) -> Result<(), Error>;

    fn spawn_new_window(
        &self,
        config: &Arc<Config>,
        fontconfig: &Rc<FontConfiguration>,
        tab: &Rc<Tab>,
    ) -> Result<(), Error>;

    fn gui_executor(&self) -> Box<Executor>;
}
