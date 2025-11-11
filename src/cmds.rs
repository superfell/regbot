use serenity::all::{
    CommandDataOption, CommandDataOptionValue, CommandInteraction, CommandOptionType, Context,
    CreateAutocompleteResponse, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, InteractionResponseFlags,
};
use serenity::async_trait;
use std::sync::{Arc, Mutex};

use crate::db::Reg;
use crate::HandlerState;

#[async_trait]
pub trait ACommand: Send + Sync {
    fn name(&self) -> &str;

    fn create(&self) -> CreateCommand;

    async fn autocomplete(&self, _ctx: Context, _a: CommandInteraction) {}

    async fn execute(&self, _ctx: Context, _a: CommandInteraction) {}
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
    fn create(&self) -> CreateCommand {
        CreateCommand::new(self.name())
            .description("Ask Reg to announce race registration info for a particular series")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "series",
                    "The series to announce",
                )
                .set_autocomplete(true)
                .required(true),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "min_reg",
                    "The minimum number of registered race entries before making an announcement.",
                )
                .min_int_value(0)
                .max_int_value(1000)
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "max_reg",
                    "Stop making announcements after this many people are registered.",
                )
                .required(false)
                .min_int_value(1)
                .max_int_value(1000),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Boolean,
                    "open",
                    "Always announce when registration opens",
                )
                .required(false),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Boolean,
                    "close",
                    "Always announce when registration closes",
                )
                .required(false),
            )
    }

    async fn autocomplete(&self, ctx: Context, autocomp: CommandInteraction) {
        for opt in &autocomp.data.options {
            if opt.name == "series" {
                let mut response = CreateAutocompleteResponse::new();
                let search_txt = opt.value.as_str().unwrap_or("").to_lowercase();
                let mut count = 0;
                {
                    let state = self.state.lock().expect("unable to lock state");
                    for season in state.seasons.values() {
                        if season.lc_name.contains(&search_txt) {
                            response = response.add_int_choice(&season.name, season.series_id);
                            count += 1;
                            if count == 25 {
                                break;
                            }
                        }
                    }
                }
                if let Err(e) = autocomp
                    .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
                    .await
                {
                    println!("Failed to send autocomp response {:?}", e);
                }
            }
        }
    }

    async fn execute(&self, ctx: Context, command: CommandInteraction) {
        let series_id = match resolve_series_id(&ctx, &command).await {
            None => return,
            Some(i) => i,
        };
        let open = resolve_option_bool(&command.data.options, "open").unwrap_or(false);
        let close = resolve_option_bool(&command.data.options, "close").unwrap_or(false);
        let maybe_min_reg = resolve_option_i64(&command.data.options, "min_reg");
        let maybe_max_reg = resolve_option_i64(&command.data.options, "max_reg");
        let result = {
            let mut st = self.state.lock().expect("couldn't lock state");
            match st.seasons.get(&series_id) {
                None => Err("unable to find series, please select one from the list"),
                Some(series) => {
                    let min_reg = maybe_min_reg.unwrap_or(series.reg_official / 2);
                    let max_reg = maybe_max_reg.unwrap_or(
                        ((series.reg_split - series.reg_official) / 2) + series.reg_official,
                    );
                    let max_reg = max_reg.max(min_reg + 1);

                    let reg = Reg {
                        guild: command.guild_id,
                        channel: command.channel_id,
                        series_id,
                        series_name: series.name.clone(),
                        min_reg,
                        max_reg,
                        open,
                        close,
                    };
                    st.db
                        .upsert_reg(&reg, &command.user.name)
                        .map(|_| {
                            format!(
                                "Okay, I will message this channel about race registrations for {}",
                                &reg
                            )
                        })
                        .map_err(|e| {
                            println!("db failed to upsert reg {:?}", e);
                            "Sorry I appear to have lost my notepad, try again later."
                        })
                }
            }
        };
        match result {
            Err(e) => respond_error(&ctx, &command, e).await,
            Ok(msg) => respond_msg(&ctx, &command, &msg).await,
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
    fn create(&self) -> CreateCommand {
        CreateCommand::new(self.name())
            .description("List the series that are being watched for this channel.")
    }
    async fn execute(&self, ctx: Context, command: CommandInteraction) {
        let regs = {
            let st = self.state.lock().expect("Unable to lock state");
            st.db.channel_regs(command.channel_id)
        };
        match regs {
            Err(e) => {
                println!("Failed to read watches {:?}", e);
                respond_error(
                    &ctx,
                    &command,
                    "Sorry, i can't find my notebook right how, try again later.",
                )
                .await;
            }
            Ok(r) => {
                if r.is_empty() {
                    respond_msg(
                        &ctx,
                        &command,
                        "No registration announcements for this channel.",
                    )
                    .await;
                } else {
                    let mut msgs = Vec::new();
                    msgs.push("Will post about race registrations for:".to_string());
                    for cr in r {
                        msgs.push(format!("\u{2981} {}", cr));
                    }
                    respond_msg(&ctx, &command, &msgs.join("\n")).await;
                }
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
    fn create(&self) -> CreateCommand {
        CreateCommand::new(self.name())
            .description("Stop reporting race registrations for a series.")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "series",
                    "The series to stop watching",
                )
                .set_autocomplete(true)
                .required(true),
            )
    }

    async fn autocomplete(&self, ctx: Context, autocomp: CommandInteraction) {
        for opt in &autocomp.data.options {
            if opt.name == "series" {
                let search_txt = opt.value.as_str().unwrap_or("").to_lowercase();
                let mut count = 0;
                let mut response = CreateAutocompleteResponse::new();
                {
                    let st = self.state.lock().expect("Unable to lock state");
                    let regs = st
                        .db
                        .channel_regs(autocomp.channel_id)
                        .expect("Failed to read db");
                    for reg in regs {
                        if reg.series_name.to_lowercase().contains(&search_txt) {
                            response = response.add_int_choice(&reg.series_name, reg.series_id);
                            count += 1;
                            if count == 25 {
                                break;
                            }
                        }
                    }
                }
                if let Err(e) = autocomp
                    .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
                    .await
                {
                    println!("Failed to send autocomp response {:?}", e);
                }
            }
        }
    }

    async fn execute(&self, ctx: Context, command: CommandInteraction) {
        let series_id = match resolve_series_id(&ctx, &command).await {
            None => return,
            Some(i) => i,
        };
        let dbr = {
            let mut st = self.state.lock().expect("Unable to lock state");
            st.db.delete_reg(command.channel_id, series_id)
        };
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

async fn resolve_series_id(ctx: &Context, command: &CommandInteraction) -> Option<i64> {
    let maybe_series_id = match &command.data.options[0].value {
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

async fn respond_msg(ctx: &Context, command: &CommandInteraction, msg: &str) {
    if let Err(e) = command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new().content(msg),
            ),
        )
        .await
    {
        println!("Failed to respond to command {}", e);
    }
}

async fn respond_error(ctx: &Context, command: &CommandInteraction, msg: &str) {
    if let Err(e) = command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .flags(InteractionResponseFlags::EPHEMERAL)
                    .content(msg),
            ),
        )
        .await
    {
        println!("Failed to respond to command {}", e);
    }
}

fn resolve_option_i64(opts: &[CommandDataOption], opt_name: &str) -> Option<i64> {
    for o in opts {
        if o.name == opt_name {
            let r = o.value.as_i64();
            if r.is_none() {
                println!("unexpected int value for {} of {:?}", opt_name, o.value);
            }
            return r;
        };
    }
    None
}

fn resolve_option_bool(opts: &[CommandDataOption], opt_name: &str) -> Option<bool> {
    for o in opts {
        if o.name == opt_name {
            let r = o.value.as_bool();
            if r.is_none() {
                println!("unexpected bool value for {} of {:?}", opt_name, o.value);
            }
            return r;
        }
    }
    None
}

pub struct HelpCommand;

const HELP_MSG:&str = "Hey there, I'm Reginald. While I sip my coffee I'll keep an eye on race registrations for you. Let me know what series you're interested in and I'll message a channel when I see some activity for that series. Use the /watch command to select a series.

You can control how many race entries are needed before i say anything with the min_reg option. I can also stop yammering on about it once there's a critical mass registered, use the max_reg option. If you want to always know when race registration opens or closes, you can use the open and close options to turn that on.

By default I'll start reporting registrations at 50% of official and stop if it reaches halfway between official and splitting.

The entry/split numbers reported at registration closed might not match exactly the race session(s) as you can't get the numbers until the end of the race.

If you forget what you asked for, you can /watching to find out. You can also /nomore if you don't care about a series anymore.";

#[async_trait]
impl ACommand for HelpCommand {
    fn name(&self) -> &str {
        "help"
    }
    fn create(&self) -> CreateCommand {
        CreateCommand::new(self.name()).description("Ask Reg what his deal is.")
    }
    async fn execute(&self, ctx: Context, command: CommandInteraction) {
        respond_msg(&ctx, &command, HELP_MSG).await;
    }
}
