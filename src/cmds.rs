use serenity::async_trait;
use serenity::model::prelude::interaction::application_command::{
    CommandDataOption, CommandDataOptionValue,
};
use serenity::model::prelude::interaction::{InteractionResponseType, MessageFlags};
use serenity::{
    builder::CreateApplicationCommands,
    model::prelude::{
        command::CommandOptionType,
        interaction::{
            application_command::ApplicationCommandInteraction,
            autocomplete::AutocompleteInteraction,
        },
    },
    prelude::Context,
};
use std::sync::{Arc, Mutex};

use crate::db::Reg;
use crate::{HandlerState};

#[async_trait]
pub trait ACommand: Send + Sync {
    fn name(&self) -> &str;

    fn create(&self, _commands: &mut CreateApplicationCommands) {}

    async fn autocomplete(&self, _ctx: Context, _a: AutocompleteInteraction) {}

    async fn execute(&self, _ctx: Context, _a: ApplicationCommandInteraction) {}
}

pub struct RegCommand {
    state: Arc<Mutex<HandlerState>>,
}
impl RegCommand {
    pub fn new(state: Arc<Mutex<HandlerState>>) -> Self {
        RegCommand { state }
    }
}
#[async_trait]
impl ACommand for RegCommand {
    fn name(&self) -> &str {
        "reg"
    }
    fn create(&self, commands: &mut CreateApplicationCommands) {
        commands
                .create_application_command(|command| {
                    command
                        .name(self.name())
                        .description("Ask Reg to announce race registration info for a particular series")
                        .create_option(|option| -> &mut serenity::builder::CreateApplicationCommandOption {
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
                                .description("The minimum number of registered race entries before making an announcement.")
                                .kind(CommandOptionType::Integer)
                                .min_int_value(0).max_int_value(1000)
                                .required(false)
                        }).create_option(|option| {
                            option.name("max_reg").description("Stop making announcements after this many people are registered.").kind(CommandOptionType::Integer).required(false).min_int_value(1).max_int_value(1000)
                        }).create_option(|option| {
                            option.name("open").description("Always announce when registration opens").kind(CommandOptionType::Boolean).required(false)
                        }).create_option(|option| {
                            option.name("close").description("Always announce when registration closes").kind(CommandOptionType::Boolean).required(false)
                        })
                });
    }

    async fn autocomplete(&self, ctx: Context, autocomp: AutocompleteInteraction) {
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

    async fn execute(&self, ctx: Context, command: ApplicationCommandInteraction) {
        let series_id = match command.data.options[0].resolved.as_ref().unwrap() {
            CommandDataOptionValue::String(x) => x.parse(),
            CommandDataOptionValue::Integer(x) => Ok(*x),
            _ => Ok(414),
        }
        .expect("Failed to parse series_id");

        let open = resolve_option_bool(&command.data.options, "open").unwrap_or(false);
        let close = resolve_option_bool(&command.data.options, "close").unwrap_or(false);
        let maybe_min_reg = resolve_option_i64(&command.data.options, "min_reg");
        let maybe_max_reg = resolve_option_i64(&command.data.options, "max_reg");
        let mut msg;
        let dbr: rusqlite::Result<usize>;
        {
            let mut st = self.state.lock().expect("couldn't lock state");
            let series = &st.seasons[&series_id];
            let min_reg = maybe_min_reg.unwrap_or(series.reg_official / 2);
            let max_reg = maybe_max_reg
                .unwrap_or(((series.reg_split - series.reg_official) / 2) + series.reg_official);

            msg = format!("Okay, I will message this channel about registration for series {} when it reaches at least {} reg, and stop after reg reaches {}.", &series.name, min_reg,max_reg);
            msg.push_str(match (open, close) {
                (true, true) => " I'll also say when registration opens and closes.",
                (true, false) => " I'll also say when registration opens.",
                (false, true) => " I'll also say when registration closes.",
                (false, false) => "",
            });
            dbr = st.db.upsert_reg(
                &Reg {
                    guild: command.guild_id,
                    channel: command.channel_id,
                    series_id,
                    min_reg,
                    max_reg,
                    open,
                    close,
                },
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
                            message.flags(MessageFlags::EPHEMERAL);
                            message
                                .content("Sorry I appear to have lost my notepad, try again later.")
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

pub struct ListCommand {
    state: Arc<Mutex<HandlerState>>,
}
impl ListCommand {
    pub fn new(state: Arc<Mutex<HandlerState>>) -> Self {
        Self { state }
    }
}
#[async_trait]
impl ACommand for ListCommand {
    fn name(&self) -> &str {
        "watching"
    }
    fn create(&self, commands: &mut CreateApplicationCommands) {
        commands.create_application_command(|command| {
            command
                .name(self.name())
                .description("List the series that are being watched for this channel.")
        });
    }
    async fn execute(&self, ctx: Context, command: ApplicationCommandInteraction) {
        let regs: rusqlite::Result<Vec<Reg>>;
        let mut series:Vec<String> = Vec::new();
        {
            let st = self.state.lock().expect("Unable to lock state");
            regs = st.db.channel_regs(command.channel_id);
            if let Ok(r) = &regs {
                series = r.iter().map(|x|st.seasons[&x.series_id].name.clone()).collect();
            }
        }
        let mut msgs = Vec::new();
        match regs {
            Err(e) => {
                println!("Failed to read watches {:?}", e);
                msgs.push(
                    "Sorry, i can't find my notebook right how, try again later.".to_string(),
                );
            }
            Ok(r) => {
                if r.is_empty() {
                    msgs.push("No registration announcements for this channel.".to_string());
                } else {
                    msgs.push("Will post about race registrations for:".to_string());
                    for (idx,x) in r.iter().enumerate() {
                        msgs.push(format!("{}", series[idx]));
                    }
                }
            }
        };
        if let Err(e) = command.create_interaction_response(&ctx.http, |r| {
            r.interaction_response_data(|d| {
                d.content(msgs.join("\n"))
            })
        }).await {
            println!("Failed to respond to /{}: {}", self.name(), e);
        }
    }
}
fn resolve_option_i64(opts: &[CommandDataOption], opt_name: &str) -> Option<i64> {
    for o in opts {
        if o.name == opt_name {
            return match o.resolved {
                Some(CommandDataOptionValue::Integer(i)) => Some(i),
                _ => {
                    println!("unexpected int value for {} of {:?}", opt_name, o.resolved);
                    None
                }
            };
        }
    }
    None
}
fn resolve_option_bool(opts: &[CommandDataOption], opt_name: &str) -> Option<bool> {
    for o in opts {
        if o.name == opt_name {
            return match o.resolved {
                Some(CommandDataOptionValue::Boolean(i)) => Some(i),
                _ => {
                    println!("unexpected bool value for {} of {:?}", opt_name, o.resolved);
                    None
                }
            };
        }
    }
    None
}
