use chrono::Utc;
use ir::{IrClient, RaceGuideEntry};
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::prelude::{ChannelId, TypingStartEvent};
use serenity::prelude::*;
use serenity::{async_trait, CacheAndHttp};
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Arc;
use tokio::spawn;
use tokio::time::Instant;

mod ir;

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    // Set a handler for the `message` event - so that whenever a new message
    // is received - the closure (or function) passed will be called.
    //
    // Event handlers are dispatched through a threadpool, and so multiple
    // events can be dispatched simultaneously.
    async fn message(&self, ctx: Context, msg: Message) {
        //println!("message was {}", msg.content);
        if msg.content == "!ping" {
            // Sending a message can fail, due to a network error, an
            // authentication error, or lack of permissions to post in the
            // channel, so log to stdout when some error happens, with a
            // description of it.
            if let Err(why) = msg.channel_id.say(&ctx.http, "Pong!").await {
                println!("Error sending message: {:?}", why);
            }
        }
    }
    async fn typing_start(&self, ctx: Context, e: TypingStartEvent) {
        println!("someone is typing {:?}", e);
        if let Err(why) = e.channel_id.say(&ctx.http, "Pong!").await {
            println!("Error sending message: {:?}", why);
        }
    }

    // Set a handler to be called on the `ready` event. This is called when a
    // shard is booted, and a READY payload is sent by Discord. This payload
    // contains data like the current user's guild Ids, current user data,
    // private channels, and more.
    //
    // In this case, just print what the current user's username is.
    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

#[tokio::main]
async fn main() {
    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");
    let ir_user = env::var("IRUSER").expect("Expected an iRacing username in the environment");
    let ir_pwd = env::var("IRPWD").expect("Expected an iRacing password in the environment");
    // Set gateway intents, which decides what events the bot will be notified about
    // let intents = GatewayIntents::GUILD_MESSAGES
    //     | GatewayIntents::DIRECT_MESSAGES
    //     | GatewayIntents::MESSAGE_CONTENT;

    // Create a new instance of the Client, logging in as a bot. This will
    // automatically prepend your bot token with "Bot ", which is a requirement
    // by Discord for bot users.
    let handler = Handler;
    let mut client = Client::builder(
        &token,
        GatewayIntents::GUILD_MESSAGES | GatewayIntents::GUILD_MESSAGE_TYPING,
    )
    .event_handler(handler)
    .await
    .expect("Err creating client");

    let http = client.cache_and_http.clone();
    spawn(iracing_loop_task(http, ir_user, ir_pwd));
    // Finally, start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}

async fn iracing_loop_task(http: Arc<CacheAndHttp>, user: String, password: String) {
    let def_backoff = tokio::time::Duration::from_secs(1);
    let max_backoff = tokio::time::Duration::from_secs(120);
    let mut backoff = def_backoff;
    let mut series_state = HashMap::new();
    loop {
        match iracing_loop(&mut series_state, http.clone(), &user, &password).await {
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
    http: Arc<CacheAndHttp>,
    user: &str,
    password: &str,
) -> anyhow::Result<()> {
    let loop_interval = tokio::time::Duration::from_secs(60);
    let client = IrClient::new(&user, &password).await?;
    if series_state.is_empty() {
        let seasons = client.seasons().await?;
        for season in seasons {
            let reg = SeriesReg::new(season);
            series_state.insert(reg.series_id(), reg);
        }
    }
    loop {
        println!("checking for race guide updates");
        let start = Instant::now();
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
        println!("starting discord announcements");
        announce(http.clone(), announcements).await;
        println!("all done for this time");
        tokio::time::sleep_until(start + loop_interval).await;
    }
}
async fn announce(http: Arc<CacheAndHttp>, msgs: Vec<(i64, String)>) {
    let x = ChannelId(1013223479992127498);
    let mut concatted = String::new();
    for msg in msgs {
        if concatted.len() + 1 + msg.1.len() > 1950 {
            let r = x.say(&http.http, &concatted).await;
            if let Err(e) = r {
                println!("announce got error: {:?}", e);
            }
            concatted.clear();
        }
        concatted.push('\n');
        concatted.push_str(&msg.1);
    }
    if !concatted.is_empty() {
        let r = x.say(&http.http, &concatted).await;
        if let Err(e) = r {
            println!("announce got error: {:?}", e);
        }
    }
}

struct SeriesReg {
    season: ir::Season,
    race_guide: Option<RaceGuideEntry>,
}
impl SeriesReg {
    fn new(s: ir::Season) -> Self {
        SeriesReg {
            season: s,
            race_guide: None,
        }
    }
    #[inline]
    fn series_id(&self) -> i64 {
        self.season.series_id
    }
    #[inline]
    fn name(&self) -> &str {
        &self.season.schedules[0].series_name.trim()
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
