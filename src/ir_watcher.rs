use chrono::{Duration, Utc};
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};
use tokio::{sync::mpsc::Sender, time::Instant};

use crate::ir::{IrClient, RaceGuideEntry, Season, Series};

#[derive(Debug)]
pub enum RaceGuideEvent {
    Seasons(HashMap<i64, SeasonInfo>),
    Announcements(HashMap<i64, Announcement>),
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
async fn update_series_info(
    client: &IrClient,
    series_state: &mut HashMap<i64, SeriesReg>,
    tx: &mut Sender<RaceGuideEvent>,
) -> anyhow::Result<()> {
    println!("checking for updated series/season info");
    let seasons = client.seasons().await?;
    let series = client.series().await?;
    let mut series_by_id = HashMap::with_capacity(series.len());
    for s in series {
        series_by_id.insert(s.series_id, s);
    }
    for season in seasons {
        let series = series_by_id.remove(&season.series_id).unwrap();
        series_state
            .entry(series.series_id)
            .or_insert_with(|| SeriesReg::new(series, season));
    }

    let season_infos: HashMap<i64, SeasonInfo> = series_state
        .iter()
        .map(|(k, v)| (*k, SeasonInfo::new(&v.series, &v.season)))
        .collect();

    println!("Sending {} series to discord bot", season_infos.len());
    if let Err(err) = tx.send(RaceGuideEvent::Seasons(season_infos)).await {
        println!("Error sending Seasons to channel {:?}", err);
    }
    Ok(())
}
async fn iracing_loop(
    series_state: &mut HashMap<i64, SeriesReg>,
    user: &str,
    password: &str,
    tx: &mut Sender<RaceGuideEvent>,
) -> anyhow::Result<()> {
    let loop_interval = tokio::time::Duration::from_secs(61);
    let client = IrClient::new(user, password).await?;
    //
    let mut series_updated = Utc::now();
    update_series_info(&client, series_state, tx).await?;
    loop {
        let start = Instant::now();
        let now_utc = Utc::now();
        if now_utc.date_naive() != series_updated.date_naive() {
            update_series_info(&client, series_state, tx).await?;
            series_updated = now_utc;
        }
        println!("checking for race guide updates");
        let guide = client.race_guide().await?;
        // the guide contains race starts for upto 3 hours, so each series may appear more than once
        // so we need to keep track of which ones we've seen and only process the first one for each series.
        let mut seen = HashSet::new();
        let mut announcements = HashMap::new();
        for e in guide.sessions {
            if seen.insert(e.series_id) {
                if let Some(sr) = series_state.get_mut(&e.series_id) {
                    if let Some(msg) = sr.update(e) {
                        announcements.insert(sr.series_id(), msg);
                    }
                }
                continue;
            }
        }
        if !announcements.is_empty() {
            match tx.send(RaceGuideEvent::Announcements(announcements)).await {
                Err(err) => println!("Failed to send RaceGuideEvent to channel {:?}", err),
                Ok(_) => println!(
                    "all done for this time, took {}ms",
                    (Instant::now() - start).as_millis()
                ),
            }
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
    pub fn new(series: &Series, _season: &Season) -> Self {
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

#[derive(Debug, Clone)]
pub enum AnnouncementType {
    Open,
    Count,
    Closed,
}

#[derive(Debug, Clone)]
pub struct Announcement {
    pub series_name: String,
    pub prev: RaceGuideEntry,
    pub curr: RaceGuideEntry,
    pub num_official: i64,
    pub num_split: i64,
    pub ann_type: AnnouncementType,
}
impl Announcement {
    fn new(
        series_name: String,
        prev: RaceGuideEntry,
        curr: RaceGuideEntry,
        num_official: i64,
        num_split: i64,
        ann_type: AnnouncementType,
    ) -> Self {
        Announcement {
            series_name,
            prev,
            curr,
            num_official,
            num_split,
            ann_type,
        }
    }
    // returns true if the number of splits has changed
    pub fn splits_changed(&self) -> bool {
        self.prev.num_splits(self.num_split) != self.curr.num_splits(self.num_split)
    }
}
impl Display for Announcement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let off = Duration::seconds(29);
        let to_start = self.curr.start_time - Utc::now();
        let split_text = |rge: &RaceGuideEntry| {
            let split_count = rge.num_splits(self.num_split);
            if rge.entry_count < self.num_official {
                "".to_string()
            } else if split_count < 2 {
                "Official! ".to_string()
            } else {
                format!("{} splits! ", split_count)
            }
        };
        match self.ann_type {
            AnnouncementType::Open => write!(
                f,
                "{}: Registration open!, {} minutes til race time",
                &self.series_name,
                (to_start + off).num_minutes()
            ),
            AnnouncementType::Count => {
                let starts_in = if to_start.num_minutes() < 1 {
                    "less than a minute! \u{1f3ce}".to_string()
                } else {
                    format!(
                        "{} minute{}",
                        (to_start + off).num_minutes(),
                        if (to_start + off).num_minutes() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )
                };
                write!(
                    f,
                    "{}: {} registered. {}Session starts in {}",
                    &self.series_name,
                    self.curr.entry_count,
                    split_text(&self.curr),
                    starts_in
                )
            }
            AnnouncementType::Closed => {
                write!(
                    f,
                    "{}: registration closed \u{26d4} {} registered {}.",
                    &self.series_name,
                    self.prev.entry_count,
                    split_text(&self.prev)
                )
            }
        }
    }
}

struct SeriesReg {
    series: Series,
    #[allow(dead_code)]
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
    fn update(&mut self, e: RaceGuideEntry) -> Option<Announcement> {
        if self.race_guide.is_none() {
            self.race_guide = Some(e);
            return None;
        }
        // reg open
        let prev = self.race_guide.take().unwrap();
        let ann = if prev.session_id.is_none() && e.session_id.is_some() {
            Some(Announcement::new(
                self.series.series_name.clone(),
                prev,
                e.clone(),
                self.series.min_starters,
                self.series.max_starters,
                AnnouncementType::Open,
            ))
        // reg count changed
        } else if prev.session_id.is_some()
            && e.session_id.is_some()
            && prev.entry_count != e.entry_count
            && (prev.entry_count > 0 || e.entry_count > 0)
        {
            Some(Announcement::new(
                self.series.series_name.clone(),
                prev,
                e.clone(),
                self.series.min_starters,
                self.series.max_starters,
                AnnouncementType::Count,
            ))
        // reg closed
        } else if prev.session_id.is_some() && e.session_id.is_none() && prev.entry_count > 0 {
            Some(Announcement::new(
                self.series.series_name.clone(),
                prev,
                e.clone(),
                self.series.min_starters,
                self.series.max_starters,
                AnnouncementType::Closed,
            ))
        } else {
            None
        };
        self.race_guide = Some(e);
        ann
    }
}
