use db::{Db, Reg};
use ir_watcher::Announcement;
use ir_watcher::{iracing_loop_task, RaceGuideEvent, SeasonInfo};
use serenity::async_trait;
use serenity::http::Http;
use serenity::model::application::command::CommandOptionType;
use serenity::model::application::interaction::application_command::CommandDataOptionValue;
use serenity::model::application::interaction::{Interaction, InteractionResponseType};
use serenity::model::gateway::Ready;
use serenity::model::id::GuildId;
use serenity::model::prelude::interaction::application_command::CommandDataOption;
use serenity::model::prelude::ChannelId;
use serenity::prelude::Context;
use serenity::prelude::EventHandler;
use serenity::prelude::GatewayIntents;
use serenity::Client;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;

mod db;
mod ir;
mod ir_watcher;

struct HandlerState {
    seasons: HashMap<i64, SeasonInfo>,
    db: Db,
}

struct Handler {
    state: Arc<Mutex<HandlerState>>,
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
}
fn resolve_option_i64(opts: &Vec<CommandDataOption>, opt_idx: usize, def_val: i64) -> i64 {
    match opts.get(opt_idx) {
        None => def_val,
        Some(ov) => match ov.resolved {
            Some(CommandDataOptionValue::Integer(i)) => i,
            _ => {
                println!("unexpected value {:?}", ov);
                def_val
            }
        },
    }
}
fn resolve_option_bool(opts: &Vec<CommandDataOption>, opt_idx: usize, def_val: bool) -> bool {
    match opts.get(opt_idx) {
        None => def_val,
        Some(ov) => match ov.resolved {
            Some(CommandDataOptionValue::Boolean(i)) => i,
            _ => {
                println!("unexpected bool value {:?}", ov.resolved);
                def_val
            }
        },
    }
}
#[async_trait]
impl EventHandler for Handler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Autocomplete(autocomp) = interaction {
            if autocomp.data.name == "reg" {
                for opt in &autocomp.data.options {
                    if opt.focused && opt.name == "series" {
                        if let Err(e) = autocomp
                            .create_autocomplete_response(&ctx.http, |response| {
                                let search_txt = match &autocomp.data.options[0].value {
                                    Some(serde_json::Value::String(s)) => s,
                                    _ => "",
                                };
                                let mut count = 0;
                                let lc_txt = search_txt.to_lowercase();
                                let state = self.state.lock().expect("unable to lock state");
                                for season in state.seasons.values() {
                                    if season.lc_name.contains(&lc_txt) {
                                        response.add_string_choice(&season.name, season.series_id);
                                        count += 1;
                                        if count == 25 {
                                            break;
                                        }
                                    }
                                }
                                response
                            })
                            .await
                        {
                            println!("Failed to send autocomp response {:?}", e);
                        }
                    }
                }
            }
        } else if let Interaction::ApplicationCommand(command) = interaction {
            if command.data.name == "reg" {
                let series_id = match command.data.options[0].resolved.as_ref().unwrap() {
                    CommandDataOptionValue::String(x) => x.parse(),
                    CommandDataOptionValue::Integer(x) => Ok(*x),
                    _ => Ok(414),
                }
                .expect("Failed to parse series_id");

                let open = resolve_option_bool(&command.data.options, 3, false);
                let close = resolve_option_bool(&command.data.options, 4, false);
                let mut msg;
                let mut dbr: rusqlite::Result<usize>;
                {
                    let mut st = self.state.lock().expect("couldn't lock state");
                    let series = &st.seasons[&series_id];
                    let min_reg =
                        resolve_option_i64(&command.data.options, 1, series.reg_official / 2);
                    let max_reg = resolve_option_i64(
                        &command.data.options,
                        2,
                        ((series.reg_split - series.reg_official) / 2) + series.reg_official,
                    );

                    msg = format!("Okay, I will message this channel about registration for series {} when it reaches at least {} reg, and stop after reg reaches {}.", &series.name, min_reg,max_reg);
                    msg.push_str(match (open, close) {
                        (true, true) => " I'll also say when registration opens and closes.",
                        (true, false) => " I'll also say when registration opens.",
                        (false, true) => " I'll also say when registration closes.",
                        (false, false) => "",
                    });
                    dbr = st.db.upsert_reg(
                        command.guild_id,
                        command.channel_id,
                        series_id,
                        min_reg,
                        max_reg,
                        open,
                        close,
                        &command.user.name,
                    );
                }
                if let Err(e) = dbr {
                    println!("db failed to upsert reg {:?}", e);
                    if let Err(why) = command
                        .create_interaction_response(&ctx.http, |response| {
                            response
                                .kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|message| {
                                    message.content(
                                        "Sorry I appear to have lost my notepad, try again later.",
                                    )
                                })
                        })
                        .await
                    {
                        println!("Cannot respond to slash command: {}", why);
                    }
                }

                if let Err(why) = command
                    .create_interaction_response(&ctx.http, |response| {
                        response
                            .kind(InteractionResponseType::ChannelMessageWithSource)
                            .interaction_response_data(|message| message.content(msg))
                    })
                    .await
                {
                    println!("Cannot respond to slash command: {}", why);
                }
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        println!("{:?}", ready.guilds);

        let guild_id = ready.guilds[0].id;
        // let guild_id = GuildId(
        //     env::var("GUILD_ID")
        //         .expect("Expected GUILD_ID in environment")
        //         .parse()
        //         .expect("GUILD_ID must be an integer"),
        // );

        let _commands = GuildId::set_application_commands(&guild_id, &ctx.http, |commands| {
            commands
                .create_application_command(|command| {
                    command.name("ping").description("A ping command")
                })
                .create_application_command(|command| {
                    command
                        .name("reg")
                        .description("Ask Reg to announce registration info for a particular series")
                        .create_option(|option| {
                            option
                                .name("series")
                                .description("The series to announce")
                                .set_autocomplete(true)
                                .kind(CommandOptionType::String)
                                .required(true)
                        })
                        .create_option(|option| {
                            option
                                .name("min_reg")
                                .description("The minimum number of registered race entries before making an announcement")
                                .kind(CommandOptionType::Integer)
                                .min_int_value(0).max_int_value(1000)
                                .required(false)
                        }).create_option(|option| {
                            option.name("max_reg").description("Stop making announcements after this many people are registered").kind(CommandOptionType::Integer).required(false).min_int_value(1).max_int_value(1000)
                        }).create_option(|option| {
                            option.name("open").description("Announce when registration opens").kind(CommandOptionType::Boolean).required(false)
                        }).create_option(|option| {
                            option.name("close").description("Announce when registration closes").kind(CommandOptionType::Boolean).required(false)
                        })
                })
        })
        .await;

        // println!(
        //     "I now have the following guild slash commands: {:#?}",
        //     commands
        // );

        // let guild_command = Command::create_global_application_command(&ctx.http, |command| {
        //     command
        //         .name("wonderful_command")
        //         .description("An amazing command")
        // })
        // .await;

        // println!(
        //     "I created the following global slash command: {:#?}",
        //     guild_command
        // );
    }
}

#[tokio::main]
async fn main() {
    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");
    let ir_user = env::var("IRUSER").expect("Expected an iRacing username in the environment");
    let ir_pwd = env::var("IRPWD").expect("Expected an iRacing password in the environment");

    // Build our client.
    let (tx, rx) = tokio::sync::mpsc::channel::<RaceGuideEvent>(2);
    let db = Db::new("regbot.db");
    if let Err(e) = db {
        println!("Failed to open db {:?}", e);
        return;
    }

    let handler = Handler {
        state: Arc::new(Mutex::new(HandlerState {
            seasons: HashMap::new(),
            db: db.unwrap(),
        })),
    };
    handler.listen_for_race_guide(token.clone(), rx);
    let mut client = Client::builder(token, GatewayIntents::non_privileged())
        .event_handler(handler)
        .await
        .expect("Error creating client");

    spawn(iracing_loop_task(ir_user, ir_pwd, tx));
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
    println!(
        "{} announcements, {} channels with watches",
        msgs.len(),
        reg.len()
    );
    // many reg may want the same series_id. and we can message a number of msgs to a single channel at once.
    for (ch, regs) in reg {
        let mut msger = Messenger::new(ch, http.as_ref());
        for reg in &regs {
            if let Some(msg) = msgs.get(&reg.series_id) {
                if reg.wants(msg) {
                    msger.add(&msg.to_string()).await;
                }
            }
        }
        msger.flush().await;
    }
}

struct Messenger<'a> {
    http: &'a Http,
    ch: ChannelId,
    buf: String,
}
impl<'a> Messenger<'a> {
    fn new(ch: ChannelId, http: &'a Http) -> Self {
        Messenger {
            ch,
            http,
            buf: String::new(),
        }
    }
    async fn add(&mut self, line: &str) {
        if self.buf.len() + 1 + line.len() > 1950 {
            self.flush().await;
        }
        //      if !self.buf.is_empty() {}
        self.buf.push_str(line);
        self.buf.push('\n')
    }
    async fn flush(&mut self) {
        if !self.buf.is_empty() {
            if let Err(e) = self.ch.say(self.http, &self.buf).await {
                println!("Failed to send message to channel {}: {:?}", self.ch, e);
            }
            self.buf.clear();
        }
    }
}
