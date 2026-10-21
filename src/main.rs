mod restore;
mod save;
mod save_command_strategies;

const RESURRECT_FILE_PREFIX: &str = "tmux_sessions";
const RESURRECT_FILE_EXTENSION: &str = "txt";

trait CommandExt {
    fn output_string(&mut self) -> anyhow::Result<String>;

    fn run(&mut self) -> anyhow::Result<std::process::ExitStatus>;
}

use std::{
    collections::HashMap,
    fs::File,
    io::{self, ErrorKind},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{LazyLock, Mutex, MutexGuard},
    time::{Duration, SystemTime},
};

use clap::Parser;

pub struct Tmux {
    socket_name: Option<String>,
}

impl Tmux {
    pub fn new(socket_name: Option<String>) -> Self {
        Self { socket_name }
    }

    fn tmux(&self) -> Command {
        let mut c = Command::new("tmux");
        if let Some(socket) = &self.socket_name {
            c.arg("-L").arg(socket);
        }
        c
    }
}

#[derive(Clone, Debug)]
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
        if cfg!(feature = "timings") {
            let time = now.elapsed().unwrap_or_default().as_secs_f64();
            Timings::get()
                .by_location
                .entry((caller.file(), caller.line()))
                .or_insert(Info::read())
                .push(time);
        }
        Ok(s)
    }

    #[track_caller]
    fn run(&mut self) -> anyhow::Result<std::process::ExitStatus> {
        let caller = std::panic::Location::caller();
        let now = SystemTime::now();
        let ret = self.spawn()?.wait()?;
        if cfg!(feature = "timings") {
            let time = now.elapsed().unwrap_or_default().as_secs_f64();
            Timings::get()
                .by_location
                .entry((caller.file(), caller.line()))
                .or_insert(Info::discard())
                .push(time);
        }
        Ok(ret)
    }
}

#[derive(clap::Parser)]
struct App {
    #[clap(short, long)]
    quiet: bool,

    #[clap(short, long)]
    file: Option<String>,

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

#[macro_export]
macro_rules! read_ifs {
    (IFS=$ifs:expr, read $line:expr, $($id:ident)*) => {
        use anyhow::Context as _;
        let line = $line;
        let mut s = line.split($ifs);
        let mut i = 0;
        $(
            i += 1;
            let $id = s
                .next()
                .ok_or(anyhow::anyhow!("format error {}:{}: expected {} (${})", file!(), line!(), stringify!($id), i))
                .with_context(|| line.to_owned())
                ?;
        )*
    };
}

fn resurrect_dir() -> anyhow::Result<PathBuf> {
    let mut data = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::var("HOME").map(PathBuf::from).map(|mut x| {
                x.push(".local");
                x.push("share");
                x
            })
        })?;
    data.push("tmux");
    data.push("resurrect");
    Ok(data)
}

fn resurrect_file_path() -> anyhow::Result<PathBuf> {
    let mut dir = resurrect_dir()?;
    let now = time::OffsetDateTime::now_local()?;
    let (year, month, day, hour, minute, second) = (
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    );
    let month: u8 = month.into();
    let filename = format!(
        "{RESURRECT_FILE_PREFIX}_{year:02}{month:02}{day:02}T{hour:02}{minute:02}{second:02}.{RESURRECT_FILE_EXTENSION}"
    );
    dir.push(filename);

    Ok(dir)
}

fn last_resurrect_file_path() -> anyhow::Result<PathBuf> {
    let mut dir = resurrect_dir()?;
    dir.push("last");
    Ok(dir)
}

fn allow_error<T>(err: std::io::Error, allowed_kind: ErrorKind, default: T) -> std::io::Result<T> {
    if err.kind() == allowed_kind {
        Ok(default)
    } else {
        Err(err)
    }
}

fn matches_last_written(buf: &[u8], last: &Path) -> io::Result<bool> {
    std::fs::read(last)
        .map(|b| buf == b)
        .or_else(|err| allow_error(err, ErrorKind::NotFound, false))
}

fn remove_old_backups() -> anyhow::Result<()> {
    let last = last_resurrect_file_path()?;
    let last_points_to = std::fs::read_link(&last)
        .map(Some)
        .or_else(|err| allow_error(err, ErrorKind::NotFound, None))?;
    let last_points_to = last_points_to
        .as_ref()
        .and_then(|x| x.file_name())
        .and_then(|x| x.to_str());

    let delete_before = SystemTime::now()
        .checked_sub(30 * Duration::from_hours(24))
        .ok_or(anyhow::anyhow!("time overflowed"))?
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();

    let mut errs = vec![];

    for ent in std::fs::read_dir(resurrect_dir()?)? {
        let ent = ent?;
        let meta = ent.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let path = ent.path();
        let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        if !name.starts_with(RESURRECT_FILE_PREFIX) || !name.ends_with(RESURRECT_FILE_EXTENSION) {
            continue;
        }
        if Some(name) == last_points_to {
            continue;
        }

        let should_delete = meta
            .mtime()
            .try_into()
            .map(|x: u64| x < delete_before)
            .unwrap_or(true);
        if !should_delete {
            continue;
        }
        if let Err(e) = std::fs::remove_file(path) {
            errs.push(e);
        }
    }

    if errs.is_empty() {
        Ok(())
    } else {
        let n = errs.len();
        let combined = errs
            .into_iter()
            .fold(anyhow::anyhow!("got {} errors", n), |x, y| x.context(y));
        Err(combined)
    }
}

fn main() -> anyhow::Result<()> {
    let mut app = App::parse();
    let subcommand = std::mem::take(&mut app.subcommand);
    let tmux = Tmux::new(app.socket_name);
    match (subcommand, app.file) {
        (Subcommand::Save, Some(file)) => {
            let file = File::create(file)?;
            let mut ctx = save::Ctx::new(file, tmux)?;
            save::save_all(&mut ctx)?
        }
        (Subcommand::Save, None) => {
            let file_path = resurrect_file_path()?;
            let last = last_resurrect_file_path()?;
            std::fs::create_dir_all(file_path.parent().unwrap())?;

            let mut buf = Vec::new();
            let mut ctx = save::Ctx::new(&mut buf, tmux)?;
            save::save_all(&mut ctx)?;

            if !matches_last_written(&buf, &last)? {
                std::fs::write(&file_path, buf)?;

                std::fs::remove_file(&last)
                    .or_else(|err| allow_error(err, ErrorKind::NotFound, ()))?;
                std::os::unix::fs::symlink(&file_path, &last)?;
            }

            remove_old_backups()?;
        }
        (Subcommand::Restore, file) => {
            let file = file
                .map(PathBuf::from)
                .map(Ok)
                .unwrap_or_else(last_resurrect_file_path)?;
            let mut ctx = restore::Ctx::new(&file, tmux)?;
            ctx.restore()?;
        }
    }

    if cfg!(feature = "timings") {
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
    }
    Ok(())
}
