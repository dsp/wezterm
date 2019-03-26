//! A Domain represents an instance of a multiplexer.
//! For example, the gui frontend has its own domain,
//! and we can connect to a domain hosted by a mux server
//! that may be local, running "remotely" inside a WSL
//! container or actually remote, running on the other end
//! of an ssh session somewhere.

use crate::config::Config;
use crate::frontend::guicommon::localtab::LocalTab;
use crate::mux::tab::Tab;
use crate::pty::cmdbuilder::CommandBuilder;
use crate::pty::{PtySize, PtySystem};
use failure::Error;
use std::rc::Rc;
use std::sync::Arc;

pub trait Domain {
    /// Spawn a new command within this domain
    fn spawn(&self, size: PtySize, command: Option<CommandBuilder>) -> Result<Rc<Tab>, Error>;
}

pub struct LocalDomain {
    pty_system: Box<PtySystem>,
    config: Arc<Config>,
}

impl LocalDomain {
    pub fn new(config: &Arc<Config>) -> Result<Self, Error> {
        let config = Arc::clone(config);
        let pty_system = config.pty.get()?;
        Ok(Self { pty_system, config })
    }
}

impl Domain for LocalDomain {
    fn spawn(&self, size: PtySize, command: Option<CommandBuilder>) -> Result<Rc<Tab>, Error> {
        let cmd = match command {
            Some(c) => c,
            None => self.config.build_prog(None)?,
        };
        let (master, slave) = self.pty_system.openpty(size)?;
        let child = slave.spawn_command(cmd)?;
        eprintln!("spawned: {:?}", child);

        let terminal = term::Terminal::new(
            size.rows as usize,
            size.cols as usize,
            self.config.scrollback_lines.unwrap_or(3500),
            self.config.hyperlink_rules.clone(),
        );

        Ok(Rc::new(LocalTab::new(terminal, child, master)))
    }
}
