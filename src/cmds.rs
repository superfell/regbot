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
use crate::HandlerState;

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
        "watch"
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
        let series_id = match resolve_series_id(&ctx, &command).await {
            None => return,
            Some(i) => i,
        };
        let msg: String;
        let open = resolve_option_bool(&command.data.options, "open").unwrap_or(false);
        let close = resolve_option_bool(&command.data.options, "close").unwrap_or(false);
        let maybe_min_reg = resolve_option_i64(&command.data.options, "min_reg");
        let maybe_max_reg = resolve_option_i64(&command.data.options, "max_reg");
        let dbr: rusqlite::Result<usize>;
        {
            let mut st = self.state.lock().expect("couldn't lock state");
            let series = &st.seasons[&series_id];
            let min_reg = maybe_min_reg.unwrap_or(series.reg_official / 2);
            let max_reg = maybe_max_reg
                .unwrap_or(((series.reg_split - series.reg_official) / 2) + series.reg_official);

            let reg = Reg {
                guild: command.guild_id,
                channel: command.channel_id,
                series_id,
                min_reg,
                max_reg,
                open,
                close,
            };
            msg = format!(
                "Okay, I will message this channel about race registrations for {}",
                reg.describe(&series.name)
            );
            dbr = st.db.upsert_reg(&reg, &command.user.name);
        }
        match dbr {
            Err(e) => {
                println!("db failed to upsert reg {:?}", e);
                respond_error(
                    &ctx,
                    &command,
                    "Sorry I appear to have lost my notepad, try again later.",
                )
                .await
            }
            Ok(_) => respond_msg(&ctx, &command, &msg).await,
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
        let mut series: Vec<String> = Vec::new();
        {
            let st = self.state.lock().expect("Unable to lock state");
            regs = st.db.channel_regs(command.channel_id);
            if let Ok(r) = &regs {
                series = r
                    .iter()
                    .map(|x| st.seasons[&x.series_id].name.clone())
                    .collect();
            }
        }
        match regs {
            Err(e) => {
                println!("Failed to read watches {:?}", e);
                respond_error(
                    &ctx,
                    &command,
                    "Sorry, i can't find my notebook right how, try again later.",
                )
                .await;
                return;
            }
            Ok(r) => {
                let mut msgs = Vec::new();
                if r.is_empty() {
                    msgs.push("No registration announcements for this channel.".to_string());
                } else {
                    msgs.push("Will post about race registrations for:".to_string());
                    for (idx, x) in r.iter().enumerate() {
                        msgs.push(format!("\u{2981} {}", x.describe(&series[idx])));
                    }
                }
                respond_msg(&ctx, &command, &msgs.join("\n")).await;
            }
        };
    }
}

pub struct RemoveCommand {
    state: Arc<Mutex<HandlerState>>,
}
impl RemoveCommand {
    pub fn new(state: Arc<Mutex<HandlerState>>) -> Self {
        Self { state }
    }
}
#[async_trait]
impl ACommand for RemoveCommand {
    fn name(&self) -> &str {
        "nomore"
    }
    fn create(&self, commands: &mut CreateApplicationCommands) {
        commands.create_application_command(|command| {
            command
                .name(self.name())
                .description("Stop reporting race registrations for a series.")
                .create_option(
                    |option| -> &mut serenity::builder::CreateApplicationCommandOption {
                        option
                            .name("series")
                            .description("The series to announce")
                            .set_autocomplete(true)
                            .kind(CommandOptionType::String)
                            .required(true)
                    },
                )
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

                        let st = self.state.lock().expect("Unable to lock state");
                        let regs = st
                            .db
                            .channel_regs(autocomp.channel_id)
                            .expect("Failed to read db");
                        for reg in regs {
                            let s = &st.seasons[&reg.series_id];
                            if s.lc_name.contains(&lc_txt) {
                                response.add_string_choice(&s.name, s.series_id);
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
        let series_id = match resolve_series_id(&ctx, &command).await {
            None => return,
            Some(i) => i,
        };
        let dbr;
        {
            let mut st = self.state.lock().expect("Unable to lock state");
            dbr = st.db.delete_reg(command.channel_id, series_id);
        }
        match dbr {
            Err(e) => {
                println!("failed to remove registration {}", e);
                respond_error(
                    &ctx,
                    &command,
                    "Sorry, I seem to have lost my notepad, please try again later.",
                )
                .await;
            }
            Ok(_) => {
                respond_msg(&ctx, &command, "Okay, I wont mention it again.").await;
            }
        }
    }
}

async fn resolve_series_id(ctx: &Context, command: &ApplicationCommandInteraction) -> Option<i64> {
    let maybe_series_id = match command.data.options[0].resolved.as_ref().unwrap() {
        CommandDataOptionValue::String(x) => x.parse(),
        CommandDataOptionValue::Integer(x) => Ok(*x),
        _ => Ok(414),
    };
    match maybe_series_id {
        Err(_) => {
            respond_error(
                ctx,
                command,
                "Please select one of the series from the autocomplete list.",
            )
            .await;
            None
        }
        Ok(sid) => Some(sid),
    }
}

async fn respond_msg(ctx: &Context, command: &ApplicationCommandInteraction, msg: &str) {
    if let Err(e) = command
        .create_interaction_response(&ctx.http, |response| {
            response
                .kind(InteractionResponseType::ChannelMessageWithSource)
                .interaction_response_data(|message| message.content(msg))
        })
        .await
    {
        println!("Failed to respond to command {}", e);
    }
}

async fn respond_error(ctx: &Context, command: &ApplicationCommandInteraction, msg: &str) {
    if let Err(e) = command
        .create_interaction_response(&ctx.http, |response| {
            response
                .kind(InteractionResponseType::ChannelMessageWithSource)
                .interaction_response_data(|message| {
                    message.flags(MessageFlags::EPHEMERAL);
                    message.content(msg)
                })
        })
        .await
    {
        println!("Failed to respond to command {}", e);
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

pub struct HelpCommand;

const HELP_MSG:&str = "Hey there, I'm Reginald. While i sip my coffee I'll keep an eye on race registrations for you. Let me know what series you're interested in and I'll message a channel when I see some activity for that series. Use the /watch command to select a series.

You can control how many race entries are needed before i say anything with the min_reg option. I can also stop yammering on about it once there's a critical mass registered, use the max_reg option. If you want to always know when race registration opens or closes, you can use the open and close options to turn that on.

By default I'll start reporting registrations at 50% of official and stop if it reaches halfway between official and splitting.

If you forget what you asked for, you can /watching to find out. You can also /nomore if you don't care about a series anymore.";

#[async_trait]
impl ACommand for HelpCommand {
    fn name(&self) -> &str {
        "help"
    }
    fn create(&self, commands: &mut CreateApplicationCommands) {
        commands.create_application_command(|command| {
            command
                .name(self.name())
                .description("Ask Reg what his deal is.")
        });
    }
    async fn execute(&self, ctx: Context, command: ApplicationCommandInteraction) {
        respond_msg(&ctx, &command, HELP_MSG).await;
    }
}
