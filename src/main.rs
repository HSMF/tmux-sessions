mod restore;
mod save;
mod save_command_strategies;

trait CommandExt {
    fn output_string(&mut self) -> anyhow::Result<String>;

    fn run(&mut self) -> anyhow::Result<std::process::ExitStatus>;
}

use std::{
    collections::HashMap,
    process::Command,
    sync::{LazyLock, Mutex, MutexGuard},
    time::SystemTime,
};

use clap::Parser;

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

fn main() -> anyhow::Result<()> {
    let mut app = App::parse();
    let subcommand = std::mem::take(&mut app.subcommand);
    match subcommand {
        Subcommand::Save => {
            let mut ctx = save::Ctx::new(app)?;
            save::save_all(&mut ctx)?
        }
        Subcommand::Restore => {
            let mut ctx = restore::Ctx::new(app)?;
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
