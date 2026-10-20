use std::{borrow::Cow, process::Command};

use anyhow::Result;

use crate::{App, CommandExt};

#[derive(Debug)]
struct ExistingPane {
    session_name: String,
    window_number: String,
    pane_index: String,
}

pub struct Ctx {
    cli: App,
    existing_panes: Vec<ExistingPane>,
    restoring_from_scratch: bool,
    restore_pane_contents: bool,
    resurrect_state: String,
    delimiter: &'static str,
    restored_session_0: bool,
    restore_list: Vec<String>,

    pane_cache: Option<Vec<ExistingPane>>,
}

fn win(session_name: &str, window_number: &str) -> String {
    format!("{session_name}:{window_number}")
}

fn win_pane(session_name: &str, window_number: &str, pane_index: &str) -> String {
    format!("{session_name}:{window_number}.{pane_index}")
}

#[allow(unused)]
#[derive(Debug, Clone, Copy)]
struct Pane<'a> {
    line_type: &'a str,         // $1
    session_name: &'a str,      // $2
    window_number: &'a str,     // $3
    window_active: &'a str,     // $4
    window_flags: &'a str,      // $5
    pane_index: &'a str,        // $6
    pane_title: &'a str,        // $7
    dir: &'a str,               // $8
    pane_active: &'a str,       // $9
    pane_command: &'a str,      // $10
    pane_full_command: &'a str, // $11
}

impl<'a> Pane<'a> {
    fn new(line: &'a str, delimiter: &str) -> Result<Self> {
        crate::read_ifs!(IFS=delimiter, read line, line_type session_name window_number window_active window_flags pane_index pane_title dir pane_active pane_command pane_full_command);
        Ok(Self {
            line_type,
            session_name,
            window_number,
            window_active,
            window_flags,
            pane_index,
            pane_title,
            dir,
            pane_active,
            pane_command,
            pane_full_command,
        })
    }

    fn win(&self) -> String {
        win(self.session_name, self.window_number)
    }
}

struct Panes<'a> {
    s: std::str::Lines<'a>,
    delimiter: &'a str,
}

impl<'a> Panes<'a> {
    fn new(s: &'a str, delimiter: &'a str) -> Self {
        Self {
            s: s.lines(),
            delimiter,
        }
    }
}

impl<'a> Iterator for Panes<'a> {
    type Item = Result<Pane<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let line = self.s.next()?;
            if !line.starts_with("pane") {
                continue;
            }
            return Some(Pane::new(line, self.delimiter));
        }
    }
}

impl Ctx {
    pub fn new(cli: App) -> Result<Self> {
        let resurrect_state = std::fs::read_to_string(&cli.file)?;
        Ok(Ctx {
            cli,
            delimiter: "\t",
            existing_panes: vec![],
            restoring_from_scratch: false,
            restore_pane_contents: false,
            restored_session_0: false,
            resurrect_state,
            restore_list: [
                "vi", "vim", "view", "nvim", "emacs", "man", "less", "more", "tail", "top", "htop",
                "irssi", "weechat", "mutt",
            ]
            .into_iter()
            .map(|x| x.to_owned())
            .collect(),
            pane_cache: None,
        })
    }

    pub fn restore(&mut self) -> Result<()> {
        self.restore_all_panes()?;
        self.handle_session_0()?;
        self.restore_window_properties()?;
        self.restore_all_pane_processes()?;
        self.restore_active_pane_for_each_window()?;
        self.restore_zoomed_windows()?;
        // TODO: restore_grouped_sessions
        Ok(())
    }
}

fn remove_first_char(s: &str) -> &str {
    let Some(ch) = s.chars().next() else { return s };
    &s[ch.len_utf8()..]
}

impl Ctx {
    fn tmux(&self) -> Command {
        let mut c = Command::new("tmux");
        if let Some(socket) = &self.cli.socket_name {
            c.arg("-L").arg(socket);
        }
        c
    }

    fn detect_if_restoring_from_scratch(&self) -> Result<bool> {
        let p = self.tmux().args(["list-panes", "-a"]).output_string()?;
        Ok(p.lines().count() == 1)
    }

    fn restore_all_panes(&mut self) -> Result<()> {
        self.restoring_from_scratch = self.detect_if_restoring_from_scratch()?;
        let state = std::mem::take(&mut self.resurrect_state);
        for line in state.lines() {
            if line.starts_with("pane") {
                self.restore_pane(line)?;
            }
        }
        self.pane_cache.take(); // invalidate cache, now there are more panes that exist
        self.resurrect_state = state;
        Ok(())
    }

    fn resolve_dir<'a>(&mut self, dir: &'a str) -> Cow<'a, str> {
        if let Some(suff) = dir.strip_prefix("~") {
            let Ok(mut home) = std::env::var("HOME") else {
                return Cow::Borrowed(dir);
            };
            home.push_str(suff);
            Cow::Owned(home)
        } else {
            Cow::Borrowed(dir)
        }
    }

    fn restore_pane(&mut self, line: &str) -> Result<()> {
        let Pane {
            session_name,
            window_number,
            pane_index,
            pane_title,
            dir,
            ..
        } = Pane::new(line, self.delimiter)?;
        eprintln!("restoring pane {session_name}:{window_number}.{pane_index}");
        let dir = remove_first_char(dir);
        let dir = self.resolve_dir(dir);
        if session_name == "0" {
            self.restored_session_0 = true;
        }
        if self.pane_exists(session_name, window_number, pane_index)? {
            if self.restoring_from_scratch {
                let pane_id = self
                    .tmux()
                    .args(["display-message", "-p", "-F", "#{pane_id}", "-t"])
                    .arg(format!("{session_name}:{window_number}"))
                    .output_string()?;
                self.new_pane(session_name, window_number, &dir, pane_index)?;
                self.tmux().args(["kill-pane", "-t", &pane_id]).run()?;
            } else {
                self.register_existing_pane(session_name, window_number, pane_index);
            }
        } else if self.window_exists(session_name, window_number)? {
            self.new_pane(session_name, window_number, &dir, pane_index)?;
        } else if self.session_exists(session_name)? {
            self.new_window(session_name, window_number, &dir, pane_index)?;
        } else {
            self.new_session(session_name, window_number, &dir, pane_index)?;
        }
        self.tmux()
            .args(["select-pane", "-t"])
            .arg(format!("{session_name}:{window_number}.{pane_index}"))
            .args(["-T", pane_title]);
        // tmux select-pane -t "$session_name:$window_number.$pane_index" -T "$pane_title"
        Ok(())
    }

    fn fill_pane_cache(&mut self) -> Result<&Vec<ExistingPane>> {
        if self.pane_cache.is_none() {
            let items = self
                .tmux()
                .args([
                    "list-panes",
                    "-a",
                    "-F",
                    "#{session_name}\t#{window_index}\t#{pane_index}",
                ])
                .output_string()?;
            let mut ret = vec![];
            for line in items.lines() {
                crate::read_ifs!(IFS="\t", read line, session_name window_number pane_index);
                let session_name = session_name.into();
                let window_number = window_number.into();
                let pane_index = pane_index.into();
                ret.push(ExistingPane {
                    session_name,
                    window_number,
                    pane_index,
                })
            }
            return Ok(self.pane_cache.insert(ret));
        }
        Ok(self.pane_cache.as_ref().unwrap())
    }

    fn pane_exists(
        &mut self,
        session_name: &str,
        window_number: &str,
        pane_index: &str,
    ) -> Result<bool> {
        Ok(self.fill_pane_cache()?.iter().any(|x| {
            x.pane_index == pane_index
                && x.session_name == session_name
                && x.window_number == window_number
        }))
    }

    fn window_exists(&mut self, session_name: &str, window_number: &str) -> Result<bool> {
        Ok(self
            .fill_pane_cache()?
            .iter()
            .any(|x| x.session_name == session_name && x.window_number == window_number))
    }

    fn session_exists(&self, session_name: &str) -> Result<bool> {
        Ok(self
            .tmux()
            .args(["has-session", "-t", session_name])
            .output()?
            .status
            .success())
    }

    fn new_session(
        &self,
        session_name: &str,
        window_number: &str,
        dir: &str,
        _pane_index: &str,
    ) -> Result<()> {
        if self.restore_pane_contents {
            todo!()
        } else {
            self.tmux()
                .args(["new-session", "-d", "-s", session_name, "-c", dir])
                .env("TMUX", "")
                .run()?;
        }
        let created_window_num = self.first_window_num()?;
        if created_window_num != window_number {
            self.tmux()
                .args(["move-window", "-s"])
                .arg(format!("{session_name}:{created_window_num}"))
                .arg("-t")
                .arg(format!("{session_name}:{window_number}"));
        }
        Ok(())
    }

    fn first_window_num(&self) -> Result<String> {
        self.tmux()
            .args(["show", "-gv", "base-index"])
            .output_string()
    }

    fn new_window(
        &self,
        session_name: &str,
        window_number: &str,
        dir: &str,
        _pane_index: &str,
    ) -> Result<()> {
        // let pane_id = format!("{session_name}:{window_number}.{pane_index}");
        if self.restore_pane_contents {
            todo!()
        } else {
            self.tmux()
                .args(["new-window", "-d", "-t"])
                .arg(format!("{session_name}:{window_number}"))
                .args(["-c", dir])
                .run()?;
        }
        Ok(())
    }

    fn new_pane(
        &self,
        session_name: &str,
        window_number: &str,
        dir: &str,
        _pane_index: &str,
    ) -> Result<()> {
        eprintln!("  new_pane: {session_name}:{window_number}.{_pane_index}");
        let win = format!("{session_name}:{window_number}");
        if self.restore_pane_contents {
            todo!()
        } else {
            self.tmux()
                .args(["split-window", "-t", &win, "-c", dir])
                .run()?;
        }

        self.tmux()
            .args(["resize-pane", "-t", &win, "-U", "999"])
            .run()?;

        Ok(())
    }

    fn register_existing_pane(
        &mut self,
        session_name: &str,
        window_number: &str,
        pane_index: &str,
    ) {
        let session_name = session_name.to_owned();
        let window_number = window_number.to_owned();
        let pane_index = pane_index.to_owned();
        self.existing_panes.push(ExistingPane {
            session_name,
            window_number,
            pane_index,
        });
    }

    fn handle_session_0(&mut self) -> Result<()> {
        if !self.restoring_from_scratch || self.restored_session_0 {
            return Ok(());
        }

        let current_session = self
            .tmux()
            .args(["display", "-p", "#{client_session}"])
            .output_string()?;
        if current_session.trim() == "0" {
            self.tmux().args(["switch-client", "-n"]).run()?;
        }

        self.tmux().args(["kill-session", "-t", "0"]).run()?;

        Ok(())
    }

    fn restore_window_properties(&mut self) -> Result<()> {
        let state = std::mem::take(&mut self.resurrect_state);
        for line in state.lines() {
            if !line.starts_with("window") {
                continue;
            }

            crate::read_ifs!(IFS=self.delimiter, read line, _line_type session_name window_number window_name _window_active _window_flags window_layout automatic_rename);

            let win = format!("{session_name}:{window_number}");
            self.tmux()
                .args(["select-layout", "-t", &win, window_layout])
                .run()?;
            let window_name = remove_first_char(window_name);
            self.tmux()
                .args(["rename-window", "-t", &win, window_name])
                .run()?;
            if automatic_rename == ":" {
                self.tmux()
                    .args(["set-option", "-u", "-t", &win, "automatic-rename"])
                    .run()?;
            } else {
                self.tmux()
                    .args([
                        "set-option",
                        "-t",
                        &win,
                        "automatic-rename",
                        automatic_rename,
                    ])
                    .run()?;
            }
        }
        self.resurrect_state = state;
        Ok(())
    }

    fn restore_all_pane_processes(&mut self) -> Result<()> {
        if !self.restore_pane_processes_enabled()? {
            return Ok(());
        }

        let state = std::mem::take(&mut self.resurrect_state);

        for pane in Panes::new(&state, self.delimiter) {
            let Pane {
                session_name,
                window_number,
                pane_index,
                dir,
                pane_full_command,
                ..
            } = pane?;
            if pane_full_command == ":" {
                continue;
            }
            let dir = remove_first_char(dir);
            let pane_full_command = remove_first_char(pane_full_command);
            self.restore_pane_process(
                pane_full_command,
                session_name,
                window_number,
                pane_index,
                dir,
            )?;
        }

        self.resurrect_state = state;

        Ok(())
    }

    fn restore_pane_processes_enabled(&self) -> Result<bool> {
        Ok(true)
    }

    fn restore_active_pane_for_each_window(&mut self) -> anyhow::Result<()> {
        let state = std::mem::take(&mut self.resurrect_state);

        for pane in Panes::new(&state, self.delimiter) {
            let Pane {
                pane_active,
                session_name,
                window_number,
                pane_index: active_pane,
                ..
            } = pane?;
            if pane_active != "1" {
                continue;
            }
            let pane_id = win_pane(session_name, window_number, active_pane);
            self.tmux().args(["select-pane", "-t", &pane_id]).run()?;
        }

        self.resurrect_state = state;
        Ok(())
    }

    fn restore_zoomed_windows(&mut self) -> anyhow::Result<()> {
        for pane in Panes::new(&self.resurrect_state, self.delimiter) {
            let pane = pane?;
            let win = pane.win();
            self.tmux().args(["resize-pane", "-t", &win, "-Z"]).run()?;
        }
        Ok(())
    }
}

// process_restore_helpers
impl Ctx {
    fn restore_pane_process(
        &mut self,
        pane_full_command: &str,
        session_name: &str,
        window_number: &str,
        pane_index: &str,
        _dir: &str,
    ) -> Result<()> {
        if !self.process_should_be_restored(
            pane_full_command,
            session_name,
            window_number,
            pane_index,
        )? {
            return Ok(());
        }
        let win = win(session_name, window_number);
        let pane = win_pane(session_name, window_number, pane_index);
        eprintln!("restoring {pane}: {pane_full_command}");

        self.tmux().args(["switch-client", "-t", &win]).run()?;
        self.tmux().args(["select-pane", "-t", pane_index]).run()?;

        // TODO: use smarter strategy
        let command = pane_full_command;

        self.tmux()
            .args(["send-keys", "-t", &pane, command, "C-m"])
            .run()?;

        Ok(())
    }

    fn process_should_be_restored(
        &mut self,
        pane_full_command: &str,
        session_name: &str,
        window_number: &str,
        pane_index: &str,
    ) -> Result<bool> {
        if self.is_pane_registered_as_existing(session_name, window_number, pane_index)
            || !self.pane_exists(session_name, window_number, pane_index)?
        {
            Ok(false)
        } else if self.restore_all_processes()?
            || self.process_on_the_restore_list(pane_full_command)
        {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn restore_all_processes(&self) -> Result<bool> {
        // TODO
        Ok(false)
    }

    fn is_pane_registered_as_existing(
        &self,
        session_name: &str,
        window_number: &str,
        pane_index: &str,
    ) -> bool {
        self.existing_panes.iter().any(|x| {
            x.session_name == session_name
                && x.window_number == window_number
                && x.pane_index == pane_index
        })
    }

    fn process_on_the_restore_list(&self, pane_full_command: &str) -> bool {
        for proc in &self.restore_list {
            let m = self.get_proc_match_element(proc);
            if self.proc_matches_full_command(pane_full_command, m) {
                return true;
            }
        }
        false
    }

    fn get_proc_match_element<'a>(&self, proc: &'a str) -> &'a str {
        match proc.split_once("->") {
            Some((_, tail)) => tail,
            None => proc,
        }
    }

    fn proc_matches_full_command(&self, pane_full_command: &str, m: &str) -> bool {
        if let Some(m) = m.strip_prefix("~") {
            pane_full_command.contains(m)
        } else {
            pane_full_command
                .strip_prefix(m)
                .is_some_and(|suff| suff.is_empty() || suff.starts_with(" "))
        }
    }
}
