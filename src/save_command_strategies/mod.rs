use std::process::Command;

use anyhow::Context;

pub struct Ps {
    cache: Option<String>,
}

impl Ps {
    pub fn new() -> Self {
        Self { cache: None }
    }

    pub fn ps(&mut self) -> anyhow::Result<&str> {
        if self.cache.is_none() {
            let out = Command::new("ps").args(["-ao", "ppid,args"]).output()?;
            let out = String::from_utf8(out.stdout).context("ps -ao ppid,args")?;
            Ok(self.cache.insert(out))
        } else {
            Ok(self.cache.as_deref().unwrap())
        }
    }
}

impl Ps {
    pub fn full_command(&mut self, pid: &str) -> anyhow::Result<String> {
        let out = self.ps()?;
        let pattern = format!("{pid} ");
        for l in out.lines() {
            let l = l.trim_start();
            if let Some(args) = l.strip_prefix(&pattern) {
                return Ok(args.to_owned());
            }
        }
        Ok(String::new())
    }
}
