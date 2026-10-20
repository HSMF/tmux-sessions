mod restore;
mod save_command_strategies;

trait CommandExt {
    fn output_string(&mut self) -> anyhow::Result<String>;

    fn run(&mut self) -> anyhow::Result<std::process::ExitStatus>;
}

use std::{
    collections::HashMap,
    fmt::Display,
    fs::File,
    io::Write,
    process::Command,
    sync::{LazyLock, Mutex, MutexGuard},
    time::SystemTime,
};

use clap::Parser;

use crate::save_command_strategies::Ps;

#[derive(Clone)]
struct Info(bool, Vec<f64>);

impl Info {
    const fn discard() -> Self {
        Info(false, Vec::new())
    }
    const fn read() -> Self {
        Info(true, Vec::new())
    }

    fn push(&mut self, v: f64) {
        self.1.push(v);
    }
}

struct Timings {
    by_location: HashMap<(&'static str, u32), Info>,
}

impl Timings {
    fn get() -> MutexGuard<'static, Self> {
        static T: LazyLock<Mutex<Timings>> = LazyLock::new(|| {
            Mutex::new(Timings {
                by_location: HashMap::new(),
            })
        });
        T.lock().unwrap()
    }
}

impl CommandExt for Command {
    #[track_caller]
    fn output_string(&mut self) -> anyhow::Result<String> {
        let caller = std::panic::Location::caller();
        let now = SystemTime::now();
        let o = self.output()?;
        let s = String::from_utf8(o.stdout)?;
        let time = now.elapsed().unwrap_or_default().as_secs_f64();
        Timings::get()
            .by_location
            .entry((caller.file(), caller.line()))
            .or_insert(Info::read())
            .push(time);
        Ok(s)
    }

    #[track_caller]
    fn run(&mut self) -> anyhow::Result<std::process::ExitStatus> {
        let caller = std::panic::Location::caller();
        let now = SystemTime::now();
        let ret = self.spawn()?.wait()?;
        let time = now.elapsed().unwrap_or_default().as_secs_f64();
        Timings::get()
            .by_location
            .entry((caller.file(), caller.line()))
            .or_insert(Info::discard())
            .push(time);
        Ok(ret)
    }
}

#[derive(clap::Parser)]
struct App {
    #[clap(short, long)]
    quiet: bool,

    #[clap(short, long)]
    file: String,

    #[clap(short = 'L')]
    socket_name: Option<String>,

    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(Default, clap::Subcommand)]
enum Subcommand {
    #[default]
    Save,
    Restore,
}

struct Ctx {
    cli: App,
    f: File,
    save_command_strategy: Ps,
    delimiter: &'static str,
}

fn format_str(parts: &[&str]) -> String {
    let mut ret = String::with_capacity(parts.iter().map(|x| x.len()).sum());

    for i in parts {
        ret.push_str(i);
    }
    ret
}

struct Line<'a> {
    ctx: &'a mut Ctx,
    need_delim: bool,
}

impl Line<'_> {
    fn write(&mut self, item: impl Display) -> anyhow::Result<()> {
        if self.need_delim {
            write!(self.ctx.f, "{}", self.ctx.delimiter)?;
        }
        write!(self.ctx.f, "{item}")?;
        self.need_delim = true;
        Ok(())
    }

    fn end(self) -> anyhow::Result<()> {
        writeln!(self.ctx.f)?;
        Ok(())
    }
}

#[macro_export]
macro_rules! read_ifs {
    (IFS=$ifs:expr, read $line:expr, $($id:ident)*) => {
        use anyhow::Context as _;
        let line = $line;
        let mut s = line.split($ifs);
        $(
            let $id = s
                .next()
                .ok_or(anyhow::anyhow!("format error {}:{}: expected {}", file!(), line!(), stringify!($id)))
                .with_context(|| line.to_owned())
                ?;
        )*
    };
}

impl Ctx {
    fn new(cli: App) -> anyhow::Result<Self> {
        let f = File::create(&cli.file)?;
        Ok(Self {
            cli,
            f,
            save_command_strategy: Ps::new(),
            delimiter: "\t",
        })
    }

    fn tmux(&self) -> Command {
        let mut c = Command::new("tmux");
        if let Some(socket) = &self.cli.socket_name {
            c.arg("-L").arg(socket);
        }
        c
    }

    fn line(&mut self) -> Line<'_> {
        Line {
            ctx: self,
            need_delim: false,
        }
    }

    fn grouped_sessions_format(&self) -> String {
        format_str(&[
            "#{session_grouped}",
            self.delimiter,
            "#{session_group}",
            self.delimiter,
            "#{session_id}",
            self.delimiter,
            "#{session_name}",
        ])
    }

    fn pane_format(&self) -> String {
        format_str(&[
            "pane",
            self.delimiter,
            "#{session_name}",
            self.delimiter,
            "#{window_index}",
            self.delimiter,
            "#{window_active}",
            self.delimiter,
            ":#{window_flags}",
            self.delimiter,
            "#{pane_index}",
            self.delimiter,
            "#{pane_title}",
            self.delimiter,
            ":#{pane_current_path}",
            self.delimiter,
            "#{pane_active}",
            self.delimiter,
            "#{pane_current_command}",
            self.delimiter,
            "#{pane_pid}",
            self.delimiter,
            "#{history_size}",
        ])
    }

    fn window_format(&self) -> String {
        format_str(&[
            "window",
            self.delimiter,
            "#{session_name}",
            self.delimiter,
            "#{window_index}",
            self.delimiter,
            ":#{window_name}",
            self.delimiter,
            "#{window_active}",
            self.delimiter,
            ":#{window_flags}",
            self.delimiter,
            "#{window_layout}",
        ])
    }

    fn state_format(&self) -> String {
        format_str(&[
            "state",
            self.delimiter,
            "#{client_session}",
            self.delimiter,
            "#{client_last_session}",
        ])
    }

    fn dump_grouped_sessions(&mut self) -> anyhow::Result<String> {
        let out = self
            .tmux()
            .args(["list-sessions", "-F"])
            .arg(self.grouped_sessions_format())
            .output()?;
        for line in out.stdout.split(|ch| *ch == b'\n') {
            if line.first() != Some(&b'1') {
                continue;
            }
            todo!("grouped sessions")
        }
        Ok(String::new())
    }

    fn fetch_and_dump_grouped_sessions(&mut self) -> anyhow::Result<()> {
        let grouped_sessions_dump = self.dump_grouped_sessions()?;
        // TODO: get_grouped_sessions(grouped_sessions_dump)
        if !grouped_sessions_dump.is_empty() {
            self.f.write_all(grouped_sessions_dump.as_bytes())?;
            self.f.write_all(b"\n")?;
        }
        Ok(())
    }

    fn dump_panes_raw(&mut self) -> anyhow::Result<String> {
        Ok(String::from_utf8(
            self.tmux()
                .args(["list-panes", "-a", "-F"])
                .arg(self.pane_format())
                .output()?
                .stdout,
        )?)
    }

    fn dump_windows_raw(&mut self) -> anyhow::Result<String> {
        Ok(String::from_utf8(
            self.tmux()
                .args(["list-windows", "-a", "-F"])
                .arg(self.window_format())
                .output()?
                .stdout,
        )?)
    }

    fn dump_panes(&mut self) -> anyhow::Result<()> {
        for line in self.dump_panes_raw()?.lines() {
            read_ifs!(IFS = &self.delimiter, read line, line_type session_name window_number window_active window_flags pane_index pane_title dir pane_active pane_command pane_pid history_size);
            // TODO: skip if grouped
            _ = history_size;
            let full_command = self.pane_full_command(pane_pid)?;
            let dir = dir.replace(" ", "\\ ");

            let mut l = self.line();
            l.write(line_type)?;
            l.write(session_name)?;
            l.write(window_number)?;
            l.write(window_active)?;
            l.write(window_flags)?;
            l.write(pane_index)?;
            l.write(pane_title)?;
            l.write(dir)?;
            l.write(pane_active)?;
            l.write(pane_command)?;
            l.write(&(String::from(":") + &full_command))?;
            l.end()?;
        }
        Ok(())
    }

    fn automatic_rename(
        &self,
        session_name: &str,
        window_index: &str,
    ) -> anyhow::Result<Option<String>> {
        let automatic_rename = self
            .tmux()
            .args(["show-window-options", "-vt"])
            .arg(format!("{session_name}:{window_index}"))
            .arg("automatic-rename")
            .output()?
            .stdout;
        let automatic_rename = String::from_utf8(automatic_rename)?;
        if automatic_rename.is_empty() {
            Ok(None)
        } else {
            Ok(Some(automatic_rename))
        }
    }

    fn dump_windows(&mut self) -> anyhow::Result<()> {
        for line in self.dump_windows_raw()?.lines() {
            read_ifs!(IFS = &self.delimiter, read line, line_type session_name window_index window_name window_active window_flags window_layout);
            // TODO: skip if grouped
            let automatic_rename = self
                .automatic_rename(session_name, window_index)?
                .unwrap_or_else(|| String::from(":"));
            let automatic_rename = automatic_rename.trim();
            let mut l = self.line();
            l.write(line_type)?;
            l.write(session_name)?;
            l.write(window_index)?;
            l.write(window_name)?;
            l.write(window_active)?;
            l.write(window_flags)?;
            l.write(window_layout)?;
            l.write(automatic_rename)?;
            l.end()?;
        }
        Ok(())
    }

    fn pane_full_command(&mut self, pane_pid: &str) -> anyhow::Result<String> {
        self.save_command_strategy.full_command(pane_pid)
    }

    fn dump_state(&mut self) -> anyhow::Result<()> {
        let o = self
            .tmux()
            .args(["display-message", "-p"])
            .arg(self.state_format())
            .output()?;
        self.f.write_all(&o.stdout)?;
        Ok(())
    }
}

fn save_all(ctx: &mut Ctx) -> anyhow::Result<()> {
    ctx.fetch_and_dump_grouped_sessions()?;
    ctx.dump_panes()?;
    ctx.dump_windows()?;
    ctx.dump_state()?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut app = App::parse();
    let subcommand = std::mem::take(&mut app.subcommand);
    match subcommand {
        Subcommand::Save => {
            let mut ctx = Ctx::new(app)?;
            save_all(&mut ctx)?
        }
        Subcommand::Restore => {
            let mut ctx = restore::Ctx::new(app)?;
            ctx.restore()?;
        }
    }
    let timings = Timings::get().by_location.clone();
    let mut means: Vec<_> = timings
        .iter()
        .map(|(loc, Info(need_result, timings))| {
            let total: f64 = timings.iter().sum();
            let mean = total / timings.len() as f64;
            let stddev = timings
                .iter()
                .map(|x| (x - mean).powi(2))
                .sum::<f64>()
                .sqrt();
            let min = timings
                .iter()
                .min_by(|a, b| f64::total_cmp(a, b))
                .unwrap_or(&f64::INFINITY);
            let max = timings
                .iter()
                .max_by(|a, b| f64::total_cmp(a, b))
                .unwrap_or(&f64::NEG_INFINITY);
            let count = timings.len();
            (loc, total, need_result, mean, stddev, min, max, count)
        })
        .collect();
    means.sort_by(|a, b| a.1.total_cmp(&b.1));

    for ((file, line), _total, need_result, mean, stddev, min, max, count) in means {
        let need_result = if *need_result { "R" } else { " " };
        println!("{file}:{line} {need_result}\t x {count}\t{mean}±{stddev} [{min}..{max}]");
    }

    Ok(())
}
