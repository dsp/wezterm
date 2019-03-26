// Don't create a new standard console window when launched from the windows GUI.
#![windows_subsystem = "windows"]

#[macro_use]
extern crate failure;
#[macro_use]
extern crate failure_derive;
#[macro_use]
pub mod log;
use failure::Error;
use std::ffi::OsString;
use structopt::StructOpt;

use std::rc::Rc;
use std::sync::Arc;

mod config;
mod frontend;
mod mux;
mod opengl;
mod server;
use crate::frontend::{FrontEnd, FrontEndSelection};
use crate::mux::domain::{Domain, LocalDomain};
use crate::mux::tab::Tab;
use crate::mux::Mux;
use crate::pty::cmdbuilder::CommandBuilder;

mod font;
use crate::font::{FontConfiguration, FontSystemSelection};

mod pty;
use pty::PtySize;
use std::env;

/// Determine which shell to run.
/// We take the contents of the $SHELL env var first, then
/// fall back to looking it up from the password database.
#[cfg(unix)]
fn get_shell() -> Result<String, Error> {
    env::var("SHELL").or_else(|_| {
        let ent = unsafe { libc::getpwuid(libc::getuid()) };

        if ent.is_null() {
            Ok("/bin/sh".into())
        } else {
            use std::ffi::CStr;
            use std::str;
            let shell = unsafe { CStr::from_ptr((*ent).pw_shell) };
            shell
                .to_str()
                .map(str::to_owned)
                .map_err(|e| format_err!("failed to resolve shell: {:?}", e))
        }
    })
}

#[cfg(windows)]
fn get_shell() -> Result<String, Error> {
    Ok(env::var("ComSpec").unwrap_or("cmd.exe".into()))
}

//    let message = "; ❤ 😍🤢\n\x1b[91;mw00t\n\x1b[37;104;m bleet\x1b[0;m.";
//    terminal.advance_bytes(message);
// !=

#[derive(Debug, StructOpt)]
#[structopt(about = "Wez's Terminal Emulator\nhttp://github.com/wez/wezterm")]
#[structopt(raw(setting = "structopt::clap::AppSettings::ColoredHelp"))]
struct Opt {
    /// Skip loading ~/.wezterm.toml
    #[structopt(short = "n")]
    skip_config: bool,

    #[structopt(subcommand)]
    cmd: Option<SubCommand>,
}

#[derive(Debug, StructOpt, Default, Clone)]
struct StartCommand {
    #[structopt(
        long = "front-end",
        raw(
            possible_values = "&FrontEndSelection::variants()",
            case_insensitive = "true"
        )
    )]
    front_end: Option<FrontEndSelection>,

    #[structopt(
        long = "font-system",
        raw(
            possible_values = "&FontSystemSelection::variants()",
            case_insensitive = "true"
        )
    )]
    font_system: Option<FontSystemSelection>,

    /// Instead of executing your shell, run PROG.
    /// For example: `wezterm start -- bash -l` will spawn bash
    /// as if it were a login shell.
    #[structopt(parse(from_os_str))]
    prog: Vec<OsString>,
}

#[derive(Debug, StructOpt, Clone)]
enum SubCommand {
    #[structopt(name = "start", about = "Start a front-end")]
    #[structopt(raw(setting = "structopt::clap::AppSettings::ColoredHelp"))]
    Start(StartCommand),

    #[structopt(name = "cli", about = "Interact with experimental mux server")]
    #[structopt(raw(setting = "structopt::clap::AppSettings::ColoredHelp"))]
    Cli(CliCommand),
}

#[derive(Debug, StructOpt, Clone)]
struct CliCommand {}

fn run_terminal_gui(config: Arc<config::Config>, opts: &StartCommand) -> Result<(), Error> {
    let font_system = opts.font_system.unwrap_or(config.font_system);
    font_system.set_default();

    let fontconfig = Rc::new(FontConfiguration::new(Arc::clone(&config), font_system));

    let cmd = if !opts.prog.is_empty() {
        let argv: Vec<&std::ffi::OsStr> = opts.prog.iter().map(|x| x.as_os_str()).collect();
        let mut builder = CommandBuilder::new(&argv[0]);
        builder.args(&argv[1..]);
        Some(builder)
    } else {
        None
    };

    let mux = Rc::new(mux::Mux::new(&config));
    Mux::set_mux(&mux);

    let front_end = opts.front_end.unwrap_or(config.front_end);
    let gui = front_end.try_new(&mux)?;

    let domain = LocalDomain::new(&config)?;

    spawn_window(&mux, &domain, &*gui, cmd, &fontconfig)?;
    gui.run_forever()
}

fn main() -> Result<(), Error> {
    // This is a bit gross.
    // In order to not to automatically open a standard windows console when
    // we run, we use the windows_subsystem attribute at the top of this
    // source file.  That comes at the cost of causing the help output
    // to disappear if we are actually invoked from a console.
    // This AttachConsole call will attach us to the console of the parent
    // in that situation, but since we were launched as a windows subsystem
    // application we will be running asynchronously from the shell in
    // the command window, which means that it will appear to the user
    // that we hung at the end, when in reality the shell is waiting for
    // input but didn't know to re-draw the prompt.
    #[cfg(windows)]
    unsafe {
        winapi::um::wincon::AttachConsole(winapi::um::wincon::ATTACH_PARENT_PROCESS)
    };

    let opts = Opt::from_args();
    let config = Arc::new(if opts.skip_config {
        config::Config::default_config()
    } else {
        config::Config::load()?
    });

    match opts
        .cmd
        .as_ref()
        .map(|c| c.clone())
        .unwrap_or_else(|| SubCommand::Start(StartCommand::default()))
    {
        SubCommand::Start(start) => {
            println!("Using configuration: {:#?}\nopts: {:#?}", config, opts);
            run_terminal_gui(config, &start)
        }
        SubCommand::Cli(_) => {
            use crate::server::client::Client;
            use crate::server::codec::*;
            let mut client = Client::new(&config)?;
            eprintln!("ping: {:?}", client.ping()?);
            let tabs = client.list_tabs()?;
            for (tab_id, title) in tabs.tabs.iter() {
                eprintln!("tab {}: {}", tab_id, title);
                let _data = client.get_coarse_tab_renderable_data(GetCoarseTabRenderableData {
                    tab_id: *tab_id,
                })?;
                // eprintln!("coarse: {:?}", data);
            }
            Ok(())
        }
    }
}

fn spawn_tab(config: &Arc<config::Config>) -> Result<Rc<Tab>, Error> {
    let domain = LocalDomain::new(config)?;
    domain.spawn(PtySize::default(), None)
}

fn spawn_window(
    mux: &Rc<Mux>,
    domain: &Domain,
    gui: &FrontEnd,
    cmd: Option<CommandBuilder>,
    fontconfig: &Rc<FontConfiguration>,
) -> Result<(), Error> {
    let tab = domain.spawn(PtySize::default(), cmd)?;
    mux.add_tab(gui.gui_executor(), &tab)?;

    gui.spawn_new_window(mux.config(), &fontconfig, &tab)
}
