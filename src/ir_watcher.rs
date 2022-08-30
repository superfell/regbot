use chrono::Utc;
use std::collections::{HashMap, HashSet};
use tokio::{sync::mpsc::Sender, time::Instant};

use crate::ir::{IrClient, RaceGuideEntry, Season, Series};

#[derive(Debug)]
pub enum RaceGuideEvent {
    Seasons(HashMap<i64, SeasonInfo>),
    Announcements(Vec<(i64, String)>),
}

pub async fn iracing_loop_task(user: String, password: String, mut tx: Sender<RaceGuideEvent>) {
    let def_backoff = tokio::time::Duration::from_secs(1);
    let max_backoff = tokio::time::Duration::from_secs(120);
    let mut backoff = def_backoff;
    let mut series_state = HashMap::new();
    loop {
        match iracing_loop(&mut series_state, &user, &password, &mut tx).await {
            Err(e) => {
                println!("Error polling iRacing {:?}", e);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
            Ok(_) => {
                panic!("iRacing poller exited with no error, should never happen");
            }
        }
    }
}
async fn iracing_loop(
    series_state: &mut HashMap<i64, SeriesReg>,
    user: &str,
    password: &str,
    tx: &mut Sender<RaceGuideEvent>,
) -> anyhow::Result<()> {
    let loop_interval = tokio::time::Duration::from_secs(60);
    let client = IrClient::new(&user, &password).await?;
    if series_state.is_empty() {
        let seasons = client.seasons().await?;
        let series = client.series().await?;
        let mut series_by_id = HashMap::with_capacity(series.len());
        for s in series {
            series_by_id.insert(s.series_id, s);
        }
        let mut season_infos = HashMap::with_capacity(series_by_id.len());
        for season in seasons {
            let series = series_by_id.remove(&season.series_id).unwrap();
            season_infos.insert(series.series_id, SeasonInfo::new(&series, &season));
            let reg = SeriesReg::new(series, season);
            series_state.insert(reg.series_id(), reg);
        }
        if let Err(err) = tx.send(RaceGuideEvent::Seasons(season_infos)).await {
            println!("Error sending Seasons to channel {:?}", err);
        }
    }
    loop {
        let start = Instant::now();
        println!("checking for race guide updates at {:?}", start);
        let guide = client.race_guide().await?;
        // the guide contains race starts for upto 3 hours, so each series may appear more than once
        // so we need to keep track of which ones we've seen and only process the first one for each series.
        let mut seen = HashSet::new();
        let mut announcements = Vec::new();
        for e in guide.sessions {
            if seen.insert(e.series_id) {
                let ann = series_state.get_mut(&e.series_id);
                if let Some(sr) = ann {
                    if let Some(msg) = sr.update(e) {
                        announcements.push((sr.series_id(), msg));
                    }
                }
                continue;
            }
        }
        match tx.send(RaceGuideEvent::Announcements(announcements)).await {
            Err(err) => println!("Failed to send RaceGuideEvent to channel {:?}", err),
            Ok(_) => println!(
                "all done for this time, took {}ms",
                (Instant::now() - start).as_millis()
            ),
        }
        tokio::time::sleep_until(start + loop_interval).await;
    }
}

#[derive(Debug, Clone)]
pub struct SeasonInfo {
    pub series_id: i64,
    pub reg_official: i64,
    pub reg_split: i64,
    pub name: String,
    pub lc_name: String,
}
impl SeasonInfo {
    pub fn new(series: &Series, season: &Season) -> Self {
        let n = &series.series_name;
        SeasonInfo {
            series_id: series.series_id,
            reg_official: series.min_starters,
            reg_split: series.max_starters,
            name: n.to_string(),
            lc_name: n.to_lowercase(),
        }
    }
}

struct SeriesReg {
    series: Series,
    season: Season,
    race_guide: Option<RaceGuideEntry>,
}
impl SeriesReg {
    fn new(series: Series, season: Season) -> Self {
        SeriesReg {
            series,
            season,
            race_guide: None,
        }
    }
    #[inline]
    fn series_id(&self) -> i64 {
        self.series.series_id
    }
    #[inline]
    fn name(&self) -> &str {
        &self.series.series_name
    }
    fn update(&mut self, e: RaceGuideEntry) -> Option<String> {
        if self.race_guide.is_none() {
            // if e.session_id.is_some() {
            //     let msg = Some(format!(
            //         "{}: Registration open!, {} minutes to race time",
            //         self.name(),
            //         (e.start_time - Utc::now()).num_minutes()
            //     ));
            //     self.race_guide = Some(e);
            //     return msg;
            // }
            self.race_guide = Some(e);
            return None;
        }
        // reg open
        let prev = self.race_guide.as_ref().unwrap();
        let ann = if prev.session_id.is_none() && e.session_id.is_some() {
            Some(format!(
                "{}: Registration open!, {} minutes to race time",
                self.name(),
                (e.start_time - Utc::now()).num_minutes()
            ))
        } else if prev.session_id.is_some()
            && e.session_id.is_some()
            && prev.entry_count != e.entry_count
            && (prev.entry_count > 0 || e.entry_count > 0)
        {
            Some(format!(
                "{}: {} registered. Session starts in {} minutes",
                self.name(),
                e.entry_count,
                (e.start_time - Utc::now()).num_minutes(),
            ))
        } else if prev.session_id.is_some() && e.session_id.is_none() && prev.entry_count > 0 {
            Some(format!("{}: Registration closed.", self.name()))
        } else {
            None
        };
        self.race_guide = Some(e);
        ann
    }
}
