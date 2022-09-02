use crate::ir::{Season, Series};
use crate::ir_watcher::{Announcement, AnnouncementType};
use rusqlite::{params, Connection, Row};
use serenity::model::prelude::{ChannelId, GuildId};
use std::collections::HashMap;
use std::fmt::Write;

#[derive(Debug, Clone)]
pub struct SeasonInfo {
    pub series_id: i64,
    pub name: String,
    pub reg_official: i64,
    pub reg_split: i64,
    pub week: i64,
    pub track_name: String,
    pub track_config: String,
    pub track_cat: Option<String>,

    pub lc_name: String,
}
impl SeasonInfo {
    pub fn new(series: &Series, _season: &Season) -> Self {
        let n = &series.series_name;
        let sc = &_season.schedules[(_season.race_week - 1) as usize];
        SeasonInfo {
            series_id: series.series_id,
            name: n.to_string(),
            reg_official: series.min_starters,
            reg_split: series.max_starters,
            week: _season.race_week,
            track_name: sc.track.track_name.clone(),
            track_config: sc
                .track
                .config_name
                .as_ref()
                .map(|c| c.clone())
                .unwrap_or_default(),
            track_cat: sc.track.category.clone(),
            lc_name: n.to_lowercase(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Reg {
    pub guild: Option<GuildId>,
    pub channel: ChannelId,
    pub series_id: i64,
    pub min_reg: i64,
    pub max_reg: i64,
    pub open: bool,
    pub close: bool,
}
impl Reg {
    pub fn wants(&self, ann: &Announcement) -> bool {
        assert_eq!(self.series_id, ann.curr.series_id);
        match ann.ann_type {
            AnnouncementType::Open => self.open,
            AnnouncementType::Closed => self.close && ann.prev.entry_count >= self.min_reg,
            AnnouncementType::Count => {
                ann.splits_changed()
                    || (ann.curr.entry_count >= self.min_reg
                        && ann.curr.entry_count <= self.max_reg)
            }
        }
    }
    pub fn describe(&self, series_name: &str) -> String {
        let mut x = String::with_capacity(series_name.len() * 2);
        write!(
            &mut x,
            "{} between {} and {} entries.",
            series_name, self.min_reg, self.max_reg
        )
        .expect("Failed to format string");
        x.push_str(match (self.open, self.close) {
            (true, true) => " I'll also say when registration opens and closes.",
            (true, false) => " I'll also say when registration opens.",
            (false, true) => " I'll also say when registration closes.",
            (false, false) => "",
        });
        x
    }
}

pub struct Db {
    con: Connection,
}

impl Db {
    pub fn new(file: &str) -> rusqlite::Result<Self> {
        let con = Connection::open(file)?;
        con.execute(
            "CREATE TABLE IF NOT EXISTS reg(
                                guild_id    integer, 
                                channel_id  integer not null, 
                                series_id   integer not null,
                                min_reg     integer not null,
                                max_reg     integer not null,
                                open        integer not null,
                                close       integer not null,
                                created_by      text,
                                created_date    text,
                                modified_date   text,
                                PRIMARY KEY(channel_id,series_id)
                            )",
            [],
        )?;
        con.execute(
            "CREATE INDEX IF NOT EXISTS idx_series_id ON reg(series_id)",
            [],
        )?;
        con.execute(
            "CREATE TABLE IF NOT EXISTS series(
                                series_id    integer primary key,
                                name         text,
                                reg_official integer,
                                reg_split    integer,
                                week         integer,
                                track_name   text,
                                track_config text,
                                traack_cat   text);",
            [],
        )?;
        Ok(Db { con })
    }
    pub fn upsert_series(&mut self, s: &SeasonInfo) -> rusqlite::Result<usize> {
        self.con.execute("INSERT INTO series(series_id,name,reg_official,reg_split,week,track_name,track_config,tracK_cat)
                VALUES (?,?,?,?,?,?,?,?) ON CONFLICT DO UPDATE SET
                    name         = excluded.name,
                    reg_official = excluded.reg_official,
                    reg_split    = excluded.reg_split,
                    week         = excluded.week,
                    track_name   = excluded.track_name,
                    track_config = excluded.tracK_config,
                    track_cat    = excluded.track_cat", 
                params![s.series_id,s.name,s.reg_official,s.reg_split,s.week,s.track_name,s.track_config,s.track_cat])
    }
    pub fn get_series(&self) -> rusqlite::Result<HashMap<i64, SeasonInfo>> {
        let mut stmt = self.con.prepare("SELECT * FROM series;")?;
        let rows = stmt.query_map([], |row| {
            Ok(SeasonInfo {
                series_id: row.get("series_id")?,
                name: row.get("name")?,
                reg_official: row.get("reg_official")?,
                reg_split: row.get("reg_split")?,
                week: row.get("week")?,
                track_name: row.get("track_name")?,
                track_config: row.get("track_config")?,
                track_cat: row.get("track_cat")?,
                lc_name: row.get::<_, String>("name")?.to_lowercase(),
            })
        })?;
        let mut res = HashMap::new();
        for row in rows {
            let s = row?;
            res.insert(s.series_id, s);
        }
        Ok(res)
    }
    pub fn upsert_reg(&mut self, reg: &Reg, created_by: &str) -> rusqlite::Result<usize> {
        self.con.execute("INSERT INTO reg(guild_id, channel_id, series_id, min_reg, max_reg, open, close, created_by, created_date)
                VALUES (?,?,?,?,?,?,?,?,datetime('now')) ON CONFLICT DO UPDATE SET
                    min_reg = excluded.min_reg,
                    max_reg = excluded.max_reg,
                    open    = excluded.open,
                    close   = excluded.close,
                    modified_date = excluded.created_date", 
                params![reg.guild.map(|g|g.0), reg.channel.0, reg.series_id,reg.min_reg, reg.max_reg, reg.open, reg.close, created_by])
    }
    pub fn delete_reg(&mut self, channel_id: ChannelId, series_id: i64) -> rusqlite::Result<usize> {
        self.con.execute(
            "DELETE FROM reg WHERE series_id=? AND channel_id=?",
            params![series_id, channel_id.0],
        )
    }
    pub fn delete_channel(&mut self, channel_id: ChannelId) -> rusqlite::Result<usize> {
        self.con
            .execute("DELETE FROM reg WHERE channel_id=?", params![channel_id.0])
    }
    pub fn delete_guild(&mut self, guild_id: GuildId) -> rusqlite::Result<usize> {
        self.con
            .execute("DELETE FROM reg WHERE guild_id=?", params![guild_id.0])
    }
    pub fn regs(&self) -> rusqlite::Result<HashMap<ChannelId, Vec<Reg>>> {
        let mut res = HashMap::new();
        self.query_regs("", |r| {
            res.entry(r.channel).or_insert_with(Vec::new).push(r)
        })?;
        Ok(res)
    }
    pub fn channel_regs(&self, ch: ChannelId) -> rusqlite::Result<Vec<Reg>> {
        let mut res = Vec::new();
        let filter = format!("WHERE channel_id={}", ch.0);
        self.query_regs(&filter, |r| res.push(r))?;
        Ok(res)
    }
    fn query_regs<F>(&self, filter: &str, mut f: F) -> rusqlite::Result<()>
    where
        F: FnMut(Reg),
    {
        let sql = format!("SELECT * FROM reg {}", filter);
        let mut stmt = self.con.prepare(&sql)?;
        for row in stmt.query_map([], to_reg)? {
            f(row?);
        }
        Ok(())
    }
}

fn to_reg(row: &Row) -> rusqlite::Result<Reg> {
    let g: Option<u64> = row.get("guild_id")?;
    let c: u64 = row.get("channel_id")?;
    Ok(Reg {
        guild: g.map(GuildId),
        channel: ChannelId(c),
        series_id: row.get("series_id")?,
        min_reg: row.get("min_reg")?,
        max_reg: row.get("max_reg")?,
        open: row.get("open")?,
        close: row.get("close")?,
    })
}
