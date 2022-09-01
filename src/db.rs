use crate::ir_watcher::{Announcement, AnnouncementType};
use rusqlite::{params, Connection, Row};
use serenity::model::prelude::{ChannelId, GuildId};
use std::collections::HashMap;
use std::fmt::Write;

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
            AnnouncementType::Closed => self.close,
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
        Ok(Db { con })
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
