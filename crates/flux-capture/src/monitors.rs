use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use windows_capture::monitor::Monitor;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorQuery {
    Index(usize),
    Name(String),
}

impl FromStr for MonitorQuery {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.parse() {
            Ok(index) => Self::Index(index),
            Err(_) => Self::Name(s.to_owned()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

impl fmt::Display for MonitorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} ({}x{} @ {} Hz)",
            self.index, self.name, self.width, self.height, self.refresh_hz
        )
    }
}

pub struct FoundMonitor {
    pub monitor: Monitor,
    pub info: MonitorInfo,
}

pub fn select<'a>(query: &MonitorQuery, monitors: &'a [MonitorInfo]) -> Result<&'a MonitorInfo> {
    let mut matches = monitors.iter().filter(|m| match query {
        MonitorQuery::Index(index) => m.index == *index,
        MonitorQuery::Name(name) => m.name.eq_ignore_ascii_case(name),
    });
    let first = matches.next().ok_or_else(|| anyhow!("no monitor matches {query:?}"))?;
    match matches.next() {
        Some(_) => Err(anyhow!("more than one monitor matches {query:?}, use an index")),
        None => Ok(first),
    }
}

pub fn enumerate() -> Result<Vec<MonitorInfo>> {
    Ok(enumerate_found()?.into_iter().map(|found| found.info).collect())
}

pub fn find(query: &MonitorQuery) -> Result<FoundMonitor> {
    let mut found = enumerate_found()?;
    let infos: Vec<MonitorInfo> = found.iter().map(|f| f.info.clone()).collect();
    let selected = select(query, &infos)?;
    let position = infos.iter().position(|info| info == selected).expect("selected from infos");
    Ok(found.swap_remove(position))
}

fn enumerate_found() -> Result<Vec<FoundMonitor>> {
    Monitor::enumerate()
        .context("enumerating monitors")?
        .into_iter()
        .map(|monitor| Ok(FoundMonitor { info: describe(&monitor)?, monitor }))
        .collect()
}

fn describe(monitor: &Monitor) -> Result<MonitorInfo> {
    Ok(MonitorInfo {
        index: monitor.index()?,
        name: monitor.name()?,
        width: monitor.width()?,
        height: monitor.height()?,
        refresh_hz: monitor.refresh_rate()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(index: usize, name: &str) -> MonitorInfo {
        MonitorInfo { index, name: name.into(), width: 1920, height: 1080, refresh_hz: 60 }
    }

    #[test]
    fn numeric_query_is_an_index() {
        assert_eq!("2".parse::<MonitorQuery>().unwrap(), MonitorQuery::Index(2));
        assert_eq!("Flux".parse::<MonitorQuery>().unwrap(), MonitorQuery::Name("Flux".into()));
    }

    #[test]
    fn selects_by_name_ignoring_case() {
        let monitors = [info(1, "DELL U2720Q"), info(2, "Flux")];
        let query = MonitorQuery::Name("flux".into());
        assert_eq!(select(&query, &monitors).unwrap().index, 2);
    }

    #[test]
    fn selects_by_index() {
        let monitors = [info(1, "A"), info(2, "B")];
        assert_eq!(select(&MonitorQuery::Index(1), &monitors).unwrap().name, "A");
    }

    #[test]
    fn refuses_missing_and_ambiguous_names() {
        let monitors = [info(1, "Generic"), info(2, "Generic")];
        assert!(select(&MonitorQuery::Name("Flux".into()), &monitors).is_err());
        assert!(select(&MonitorQuery::Name("Generic".into()), &monitors).is_err());
    }
}
