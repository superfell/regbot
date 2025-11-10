use cmds::{ACommand, HelpCommand, ListCommand, RegCommand, RemoveCommand};
use db::{Db, Reg, SeasonInfo};
use ir_watcher::Announcement;
use ir_watcher::{iracing_loop_task, RaceGuideEvent};
use serenity::async_trait;
use serenity::http::Http;
use serenity::model::application::interaction::Interaction;
use serenity::model::gateway::Ready;
use serenity::model::prelude::{ChannelId, Guild, GuildChannel, GuildId, UnavailableGuild};
use serenity::prelude::Context;
use serenity::prelude::EventHandler;
use serenity::prelude::GatewayIntents;
use serenity::Client;
use std::collections::HashMap;
use std::env;
use std::panic::{set_hook, take_hook};
use std::process::abort;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;

mod cmds;
mod db;
mod ir;
mod ir_watcher;

pub struct HandlerState {
    seasons: HashMap<i64, SeasonInfo>,
    db: Db,
}

struct Handler {
    state: Arc<Mutex<HandlerState>>,
    commands: Vec<Box<dyn ACommand>>,
}

impl Handler {
    fn listen_for_race_guide(&self, token: String, rx: Receiver<RaceGuideEvent>) {
        let state = self.state.clone();
        spawn(Self::listen_task(state, token, rx));
    }
    async fn listen_task(
        state: Arc<Mutex<HandlerState>>,
        token: String,
        mut rx: Receiver<RaceGuideEvent>,
    ) {
        let http = Http::new(&token);
        loop {
            let e = rx.recv().await;
            if let Some(evt) = e {
                match evt {
                    RaceGuideEvent::Announcements(msgs) => {
                        let reg;
                        {
                            let st = state.lock().expect("Unable to lock state");
                            reg = st.db.regs().expect("query failed");
                        }
                        announce(&http, reg, msgs).await;
                    }
                    RaceGuideEvent::Seasons(s) => {
                        let mut st = state.lock().expect("Unable to lock state");
                        st.seasons = s;
                    }
                }
            }
        }
    }
    async fn install_commands(&self, ctx: &Context, guild_id: GuildId) {
        println!("Installing commands for guild {}", guild_id);
        let _commands = guild_id
            .set_application_commands(&ctx.http, |commands| {
                for c in &self.commands {
                    c.create(commands);
                }
                commands
            })
            .await;
        if let Err(e) = _commands {
            println!("Failed to install commands {:?}", e);
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Autocomplete(autocomp) = interaction {
            for c in &self.commands {
                if autocomp.data.name == c.name() {
                    c.autocomplete(ctx, autocomp).await;
                    break;
                }
            }
        } else if let Interaction::ApplicationCommand(command) = interaction {
            for c in &self.commands {
                if command.data.name == c.name() {
                    c.execute(ctx, command).await;
                    break;
                }
            }
        }
    }
    async fn guild_delete(
        &self,
        _ctx: Context,
        incomplete: UnavailableGuild,
        _full: Option<Guild>,
    ) {
        // delete any reg for this guild if the unavailable flag is false.
        println!(
            "guild delete guild_id:{} / incomplete:{}",
            incomplete.id, incomplete.unavailable
        );
        if !incomplete.unavailable {
            let mut st = self.state.lock().expect("Unable to locks state");
            if let Err(e) = st.db.delete_guild(incomplete.id) {
                println!("Failed to delete guild {} :{:?}", incomplete.id, e);
            }
        }
    }
    async fn channel_delete(&self, _ctx: Context, _channel: &GuildChannel) {
        println!(
            "channel delete guild {} channel{}",
            _channel.guild_id, _channel.id
        );
        let mut st = self.state.lock().expect("Unable to lock state");
        if let Err(e) = st.db.delete_channel(_channel.id) {
            println!(
                "Failed to delete reg entries for channel id {} {:?}",
                _channel.id, e
            );
        }
    }
    async fn guild_create(&self, ctx: Context, guild: Guild, _is_new: bool) {
        // create commands in guild
        println!("guild create {}/{}", guild.id, _is_new);
        self.install_commands(&ctx, guild.id).await;
    }

    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        println!("{:?}", ready.guilds);
    }
}

#[tokio::main]
async fn main() {
    // If something goes wrong, have the process panic and systemd restart the process.
    set_abort_on_panic();
    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");
    let ir_user = env::var("IRUSER").expect("Expected an iRacing username in the environment");
    let ir_pwd = env::var("IRPWD").expect("Expected an iRacing password in the environment");
    let ir_client =
        env::var("IRCLIENT").expect("Expected an iRacing client seceret in the environment");

    // Build our client.
    let db = Db::new("regbot.db");
    if let Err(e) = db {
        println!("Failed to open db {:?}", e);
        return;
    }
    let state = Arc::new(Mutex::new(HandlerState {
        seasons: HashMap::new(),
        db: db.unwrap(),
    }));
    let handler = Handler {
        state: state.clone(),
        commands: vec![
            Box::new(RegCommand::new(state.clone())),
            Box::new(ListCommand::new(state.clone())),
            Box::new(RemoveCommand::new(state.clone())),
            Box::new(HelpCommand),
        ],
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<RaceGuideEvent>(2);
    handler.listen_for_race_guide(token.clone(), rx);
    spawn(iracing_loop_task(
        ir_user,
        ir_pwd,
        ir_client,
        tx,
        state.clone(),
    ));

    let mut client = Client::builder(token, GatewayIntents::non_privileged())
        .event_handler(handler)
        .await
        .expect("Error creating client");

    // Finally, start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}

async fn announce(
    http: impl AsRef<Http>,
    reg: HashMap<ChannelId, Vec<Reg>>,
    msgs: HashMap<i64, Announcement>,
) {
    // many reg may want the same series_id. and we can message a number of msgs to a single channel at once.
    let reg_len = reg.len();
    let mut sent = 0;
    for (ch, regs) in reg {
        let mut msger = Messenger::new(ch, http.as_ref());
        for reg in &regs {
            if let Some(msg) = msgs.get(&reg.series_id) {
                if reg.wants(msg) {
                    msger.add(&msg.to_string()).await;
                    sent += 1;
                }
            }
        }
        msger.flush().await;
    }
    println!(
        "{} announcements, {} channels with watches, sent {} announcements",
        msgs.len(),
        reg_len,
        sent,
    );
}

pub struct Messenger<'a> {
    http: &'a Http,
    ch: ChannelId,
    buf: String,
}
impl<'a> Messenger<'a> {
    pub fn new(ch: ChannelId, http: &'a Http) -> Self {
        Messenger {
            ch,
            http,
            buf: String::new(),
        }
    }
    pub async fn add(&mut self, line: &str) {
        if self.buf.len() + 1 + line.len() > 1950 {
            self.flush().await;
        }
        //      if !self.buf.is_empty() {}
        self.buf.push_str(line);
        self.buf.push('\n')
    }
    pub async fn flush(&mut self) {
        if !self.buf.is_empty() {
            if let Err(e) = self.ch.say(self.http, &self.buf).await {
                println!("Failed to send message to channel {}: {:?}", self.ch, e);
            }
            self.buf.clear();
        }
    }
}

fn set_abort_on_panic() {
    let default_panic = take_hook();
    set_hook(Box::new(move |info| {
        eprintln!("\x1b[1;31m=== PANIC (exiting) ===\x1b[0m");
        default_panic(info);
        abort();
    }));
}
