use anyhow::anyhow;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const IR_API: &str = "https://members-ng.iracing.com/data";

pub struct IrClient {
    client: reqwest::Client,
}

impl IrClient {
    pub async fn new(username: &str, password: &str) -> Result<IrClient, anyhow::Error> {
        let c = reqwest::Client::builder().cookie_store(true).build()?;

        let mut hasher = Sha256::new();
        let normalized = username.trim().to_lowercase();
        hasher.update(format!("{password}{normalized}"));
        let encoded_auth = base64::encode(hasher.finalize());

        let mut map = HashMap::new();
        map.insert("email", username);
        map.insert("password", &encoded_auth);
        let req = c.post("https://members-ng.iracing.com/auth").json(&map);

        let res = req.send().await?;
        if !res.status().is_success() {
            println!("auth error: status {}", res.status());
            let body = res.text().await?;
            println!("{}", body);
            return Err(anyhow!("failed to authenticate: {}", body));
        }
        let _body = res.text().await?;
        Ok(IrClient { client: c })
    }

    // returns the parsed result of the supplied url, dealing with the additional
    // "link" extra resolution needed by the iracing API.
    pub async fn fetch<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, anyhow::Error> {
        let u = format!("{}/{}", IR_API, path);
        let req = self.client.get(u.clone());
        let res = req.send().await?;
        if !res.status().is_success() {
            if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let limit = res.headers().get("x-ratelimit-limit");
                let remaining = res.headers().get("x-ratelimit-remaining");
                let reset = res.headers().get("x-ratelimit-reset");
                println!(
                    "got rated limited\nlimit:{:?} remaining:{:?} reset:{:?}",
                    limit, remaining, reset
                );
            }

            return Err(anyhow!(
                "http error {} for {}\n{}",
                res.status(),
                u,
                res.text().await?
            ));
        }
        let lnk: Link = res.json().await?;
        let req = self.client.get(&lnk.link);
        match req.send().await?.json().await {
            Ok(r) => Ok(r),
            Err(e) => {
                // provide better error
                let req = self.client.get(&lnk.link);
                let txt = req.send().await?.text().await;
                if let Ok(rb) = txt {
                    println!("error {:?} response body\n{}", e, rb);
                }
                Err(anyhow!("request failed {:?}", e))
            }
        }
    }

    #[allow(dead_code)]
    pub async fn season_list(&self, year: i64, quarter: i64) -> Result<SeasonList, anyhow::Error> {
        assert!((1..=4).contains(&quarter));
        self.fetch(&format!(
            "season/list?season_year={}&season_quarter={}",
            year, quarter
        ))
        .await
    }
    pub async fn race_guide(&self) -> Result<RaceGuide, anyhow::Error> {
        self.fetch("/season/race_guide").await
    }
    pub async fn seasons(&self) -> Result<Vec<Season>, anyhow::Error> {
        self.fetch("series/seasons?include_series=false").await
    }
    pub async fn series(&self) -> Result<Vec<Series>, anyhow::Error> {
        self.fetch("/series/get").await
    }
}

/// JSON types

#[derive(Serialize, Deserialize, Debug)]
struct Link {
    pub link: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct SeasonList {
    season_quarter: i64,
    season_year: i64,
    seasons: Vec<SeasonBasic>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct SeasonBasic {
    season_id: i64,
    series_id: i64,
    season_name: String,
    series_name: String,
    official: bool,
    season_year: i64,
    season_quarter: i64,
    license_group: i64,
    fixed_setup: bool,
    driver_changes: bool,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct RaceGuide {
    pub subscribed: bool,
    pub sessions: Vec<RaceGuideEntry>,
    pub block_begin_time: String,
    pub block_end_time: String,
    pub success: bool,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct RaceGuideEntry {
    pub season_id: i64,
    pub start_time: DateTime<Utc>,
    pub super_session: bool,
    pub series_id: i64,
    pub race_week_num: i64,
    pub end_time: String,
    pub session_id: Option<i64>,
    pub entry_count: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Season {
    pub active: bool,
    pub official: bool,
    pub start_date: DateTime<Utc>,
    pub race_week: i64,
    pub max_weeks: i64,
    pub season_id: i64,
    pub season_quarter: i64,
    pub season_year: i64,
    pub series_id: i64,
    pub season_name: String,
    pub schedules: Vec<Schedule>,
}
impl Season {
    pub fn series_name(&self) -> &str {
        &self.schedules[0].series_name
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Schedule {
    pub series_id: i64,
    pub season_id: i64,
    pub race_week_num: i64,
    pub series_name: String,
    pub season_name: String,
    pub track: Track,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Track {
    pub track_id: i64,
    pub track_name: String,
    pub config_name: Option<String>,
    //    category_id: i64,
    pub category: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Series {
    pub category: String,
    pub category_id: i64,
    pub eligible: bool,
    pub max_starters: i64,
    pub min_starters: i64,
    pub oval_caution_type: i64,
    pub road_caution_type: i64,
    pub search_filters: String,
    pub series_id: i64,
    pub series_name: String,
    pub series_short_name: String,
}
