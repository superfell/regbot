use anyhow::anyhow;
use base64::encode;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const AUTH_URL: &str = "https://oauth.iracing.com/oauth2/token";
const IR_API: &str = "https://members-ng.iracing.com/data";
const IR_CLIENT: &str = "regbot";
const EXPIRY_BUFFER: Duration = Duration::from_secs(30);

pub struct IrClient {
    client: reqwest::Client,
    masked_client_secret: String,
    auth: Mutex<Auth>,
}

impl IrClient {
    pub async fn new(
        username: &str,
        password: &str,
        client_secret: &str,
    ) -> anyhow::Result<IrClient> {
        let c = reqwest::Client::builder().build()?;
        let masked_pwd = Self::mask(password, username);
        let masked_client = Self::mask(client_secret, IR_CLIENT);
        let mut params = HashMap::new();
        params.insert("grant_type", "password_limited");
        params.insert("client_secret", &masked_client);
        params.insert("username", username);
        params.insert("password", &masked_pwd);
        params.insert("scope", "iracing.auth");
        let auth = Self::token(&c, params).await?;
        Ok(IrClient {
            client: c,
            auth: Mutex::new(auth),
            masked_client_secret: masked_client,
        })
    }

    // returns a new access token
    async fn refresh(&self) -> anyhow::Result<String> {
        let mut params = HashMap::new();
        let t = self.auth.lock().unwrap().refresh.token.clone();
        params.insert("grant_type", "refresh_token");
        params.insert("client_secret", &self.masked_client_secret);
        params.insert("refresh_token", &t);
        let auth = Self::token(&self.client, params).await?;
        let access = auth.access.token.clone();
        *self.auth.lock().unwrap() = auth;
        Ok(access)
    }

    // maka a call to the oauth token endpoint
    async fn token(client: &Client, mut params: HashMap<&str, &str>) -> anyhow::Result<Auth> {
        params.insert("client_id", IR_CLIENT);
        let req = client.post(AUTH_URL).form(&params);
        let start = Instant::now();
        let res = req.send().await?;
        if !res.status().is_success() {
            println!("token error: status {}", res.status());
            let body = res.text().await?;
            println!("{}", body);
            return Err(anyhow!("failed to refresh access token: {}", body));
        }
        println!("got response from token API");
        let auth_info: AuthResult = res.json().await?;
        let access = Token {
            token: auth_info.access_token.clone(),
            expires: start + Duration::from_secs(auth_info.expires_in) - EXPIRY_BUFFER,
        };
        let refresh = Token {
            token: auth_info.refresh_token,
            expires: start + Duration::from_secs(auth_info.refresh_token_expires_in)
                - EXPIRY_BUFFER,
        };
        Ok(Auth { access, refresh })
    }

    fn mask(secret: &str, id: &str) -> String {
        let mut hasher = Sha256::new();
        let normalized_id = id.trim().to_lowercase();
        hasher.update(format!("{secret}{normalized_id}"));
        encode(hasher.finalize())
    }

    // returns a current access token, making a call to refresh it if needed.
    async fn access_token(&self) -> anyhow::Result<String> {
        let t = {
            let a = self.auth.lock().unwrap();
            if a.access.expires < Instant::now() {
                Err(())
            } else {
                Ok(a.access.token.clone())
            }
        };
        match t {
            Err(_) => self.refresh().await,
            Ok(t) => Ok(t),
        }
    }

    // returns the parsed result of the supplied url, dealing with the additional
    // "link" extra resolution needed by the iracing API.
    pub async fn fetch<T: serde::de::DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let access_token = self.access_token().await?;
        let u = format!("{}/{}", IR_API, path);
        println!("starting iRacing request to {u}");
        let req = self
            .client
            .get(u.clone())
            .header("Authorization", format!("bearer {access_token}"));
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
        println!("starting iRacing request to {}", &lnk.link);
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
    pub async fn season_list(&self, year: i64, quarter: i64) -> anyhow::Result<SeasonList> {
        assert!((1..=4).contains(&quarter));
        self.fetch(&format!(
            "season/list?season_year={}&season_quarter={}",
            year, quarter
        ))
        .await
    }
    pub async fn race_guide(&self) -> anyhow::Result<RaceGuide> {
        self.fetch("season/race_guide").await
    }
    pub async fn seasons(&self) -> anyhow::Result<Vec<Season>> {
        self.fetch("series/seasons?include_series=false").await
    }
    pub async fn series(&self) -> anyhow::Result<Vec<Series>> {
        self.fetch("series/get").await
    }
}

struct Auth {
    access: Token,
    refresh: Token,
}

struct Token {
    token: String,
    expires: Instant,
}

/// JSON types

#[derive(Deserialize, Debug)]
struct AuthResult {
    access_token: String,
    expires_in: u64,
    refresh_token: String,
    refresh_token_expires_in: u64,
}

#[derive(Serialize, Deserialize, Debug)]
struct Link {
    pub link: String,
    pub expires: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SeasonList {
    season_quarter: i64,
    season_year: i64,
    seasons: Vec<SeasonBasic>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
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

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RaceGuide {
    pub subscribed: bool,
    pub sessions: Vec<RaceGuideEntry>,
    pub block_begin_time: String,
    pub block_end_time: String,
    pub success: bool,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
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
impl RaceGuideEntry {
    pub fn num_splits(&self, split_at: i64) -> i64 {
        1 + ((self.entry_count - 1) / split_at)
    }
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

#[allow(dead_code)]
#[derive(Deserialize, Clone, Debug)]
pub struct Series {
    pub category: String,
    pub category_id: i64,
    pub eligible: bool,
    pub max_starters: i64,
    pub min_starters: i64,
    pub oval_caution_type: i64,
    pub road_caution_type: i64,
    pub search_filters: Option<String>,
    pub series_id: i64,
    pub series_name: String,
    pub series_short_name: String,
}
