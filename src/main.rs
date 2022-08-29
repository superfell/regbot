use std::env;
use std::sync::Arc;
use std::sync::Mutex;

use ir::Season;
use ir_watcher::{iracing_loop_task, RaceGuideEvent};
use regex::RegexBuilder;
use serenity::async_trait;
use serenity::http::Http;
use serenity::model::application::command::CommandOptionType;
use serenity::model::application::interaction::application_command::CommandDataOptionValue;
use serenity::model::application::interaction::{Interaction, InteractionResponseType};
use serenity::model::gateway::Ready;
use serenity::model::id::GuildId;
use serenity::model::prelude::ChannelId;
use serenity::prelude::Context;
use serenity::prelude::EventHandler;
use serenity::prelude::GatewayIntents;
use serenity::Client;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;

mod ir;
mod ir_watcher;

#[derive(Default)]
struct HandlerState {
    // rx: Arc<tokio::sync::Mutex<Receiver<RaceGuideEvent>>>,
    seasons: Vec<Season>,
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
                    RaceGuideEvent::Announcements(msgs) => announce(&http, msgs).await,
                    RaceGuideEvent::Seasons(s) => {
                        let mut asb = state.lock().expect("Unable to lock state");
                        asb.seasons = s;
                    }
                }
            }
        }
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
                                let re = RegexBuilder::new(&regex::escape(search_txt))
                                    .case_insensitive(true)
                                    .build()
                                    .unwrap();
                                let state = self.state.lock().expect("unable to lock state");
                                let mut count = 0;
                                for season in &state.seasons {
                                    if re.is_match(&season.season_name) {
                                        response.add_string_choice(
                                            &season.season_name,
                                            season.series_id,
                                        );
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
            println!("Received command interaction: {:#?}", command);

            let content = match command.data.name.as_str() {
                "ping" => "Hey, I'm alive!".to_string(),
                "id" => {
                    let options = command
                        .data
                        .options
                        .get(0)
                        .expect("Expected user option")
                        .resolved
                        .as_ref()
                        .expect("Expected user object");

                    if let CommandDataOptionValue::User(user, _member) = options {
                        format!("{}'s id is {}", user.tag(), user.id)
                    } else {
                        "Please provide a valid user".to_string()
                    }
                }
                "attachmentinput" => {
                    let options = command
                        .data
                        .options
                        .get(0)
                        .expect("Expected attachment option")
                        .resolved
                        .as_ref()
                        .expect("Expected attachment object");

                    if let CommandDataOptionValue::Attachment(attachment) = options {
                        format!(
                            "Attachment name: {}, attachment size: {}",
                            attachment.filename, attachment.size
                        )
                    } else {
                        "Please provide a valid attachment".to_string()
                    }
                }
                _ => "not implemented :(".to_string(),
            };

            if let Err(why) = command
                .create_interaction_response(&ctx.http, |response| {
                    response
                        .kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|message| message.content(content))
                })
                .await
            {
                println!("Cannot respond to slash command: {}", why);
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

        let commands = GuildId::set_application_commands(&guild_id, &ctx.http, |commands| {
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
    let handler = Handler {
        state: Arc::new(Mutex::new(HandlerState {
            seasons: Vec::new(),
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

async fn announce(http: impl AsRef<Http>, msgs: Vec<(i64, String)>) {
    println!("{} announcements", msgs.len());
    let x = ChannelId(1013223479992127498);
    let mut concatted = String::new();
    for msg in msgs {
        if concatted.len() + 1 + msg.1.len() > 1950 {
            let r = x.say(&http, &concatted).await;
            if let Err(e) = r {
                println!("announce got error: {:?}", e);
            }
            concatted.clear();
        }
        concatted.push('\n');
        concatted.push_str(&msg.1);
    }
    if !concatted.is_empty() {
        let r = x.say(&http, &concatted).await;
        if let Err(e) = r {
            println!("announce got error: {:?}", e);
        }
    }
}
