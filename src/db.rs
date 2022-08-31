use rusqlite::{params, Connection};
use serenity::model::prelude::{ChannelId, GuildId};
use std::collections::{HashMap, HashSet};

use crate::ir_watcher::{Announcement, AnnouncementType};

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
            AnnouncementType::RegOpen => self.open,
            AnnouncementType::RegClosed => self.close,
            AnnouncementType::RegCount => {
                ann.curr.entry_count >= self.min_reg && ann.curr.entry_count <= self.max_reg
            }
        }
    }
}

pub struct Db {
    con: Connection,
}

impl Db {
    pub fn new(file: &str) -> rusqlite::Result<Self> {
        let mut con = Connection::open(file)?;
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
        Ok(Db { con: con })
    }
    pub fn upsert_reg(
        &mut self,
        guild_id: Option<GuildId>,
        channel_id: ChannelId,
        series_id: i64,
        min_reg: i64,
        max_reg: i64,
        open: bool,
        close: bool,
        created_by: &str,
    ) -> rusqlite::Result<usize> {
        self.con.execute("INSERT INTO reg(guild_id, channel_id, series_id, min_reg, max_reg, open, close, created_by, created_date)
                VALUES (?,?,?,?,?,?,?,?,datetime('now')) ON CONFLICT DO UPDATE SET
                    min_reg = excluded.min_reg,
                    max_reg = excluded.max_reg,
                    open    = excluded.open,
                    close   = excluded.close,
                    modified_date = excluded.created_date", params![guild_id.map(|g|g.0), channel_id.0, series_id, min_reg, max_reg, open, close, created_by])
    }
    pub fn delete_reg(&mut self, channel_id: ChannelId, series_id: i64) -> rusqlite::Result<usize> {
        self.con.execute(
            "DELETE FROM reg WHERE series_id=? AND channel_id=?",
            params![channel_id.0, series_id],
        )
    }
    pub fn series_ids(&self) -> rusqlite::Result<HashSet<i64>> {
        let mut stmt = self.con.prepare("SELECT distinct series_id FROM reg")?;
        let mut res = HashSet::new();
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        for sid in rows {
            res.insert(sid?);
        }
        Ok(res)
    }
    pub fn regs(&self) -> rusqlite::Result<HashMap<ChannelId, Vec<Reg>>> {
        //
        let mut stmt = self.con.prepare("SELECT * FROM reg")?;
        let mut res = HashMap::new();
        let rows = stmt.query_map([], |row| {
            let g: Option<u64> = row.get("guild_id")?;
            let c: u64 = row.get("channel_id")?;
            Ok(Reg {
                guild: g.map(|g| GuildId(g)),
                channel: ChannelId(c),
                series_id: row.get("series_id")?,
                min_reg: row.get("min_reg")?,
                max_reg: row.get("max_reg")?,
                open: row.get("open")?,
                close: row.get("close")?,
            })
        })?;
        for row in rows {
            let r = row?;
            res.entry(r.channel).or_insert_with(|| Vec::new()).push(r);
        }
        Ok(res)
    }
}
